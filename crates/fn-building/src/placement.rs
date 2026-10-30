//! Build targeting (where the preview goes), placement validation (the structural-grid query) and placing.
//!
//! Translated from the 1.8 exe (addresses in the comments). Space: the level's UE world space (cm, Z up), see `crate::grid`.
//!
//! ## Targeting (`AFortPlayerController` vfunc_688 @140834290, called by the preview update @140835750)
//! 1. `Cam`, `Dir` = the camera view point (`APlayerCameraManager::GetCameraViewPoint`); `D` = |pawn location - Cam|.
//! 2. `P0 = Cam + Dir * D` (the view ray at the pawn's distance); `P1 = Cam + Dir * (D + |BuildingPlacementDistance|)`.
//! 3. If `P1` is outside the box `P0 +- BuildingPlacementDistance`, it is moved to where the segment P1 -> P0 enters the box
//!    (`FMath::LineExtentBoxIntersection`).
//! 4. `BuildingPlaceTrace` (`FUN_140833cb0`): line trace P0 -> P1; the first hit that is not start-penetrating, on a building actor that is
//!    not a Prop (4) or Container (9), or on a non-building static mesh component, gives `ImpactPoint + ImpactNormal * 10`. Landscape and
//!    props never stop it (only `UStaticMeshComponent`s of non-building actors count). Here: the placed pieces.
//! 5. Snap (`crate::grid::snap_transform`) with the view rotation and `BuildPreviewRotationIterations`, then the structural query.
//! 6. Query = NoStructuralSupport (3) and the preview's current centroid differs from `LastBuildPreviewGridSnapCentroid`: retry with the
//!    rotation iterations +1 (view yaw - snapped yaw + 90 normalised >= 0) or +3; CanAdd -> take it and keep the new iteration count.
//! 7. Still unsupported: try one level up / down along the sign of `Dir.Z` (+-`VertSnapGridSize`), first toward the view.
//!
//! ## Validation (`AFortPlayerController` vfunc_689 @140816430 -> `FUN_1404ddf50`, `EFortStructuralGridQueryResults`)
//! * ExistingActor (1): the grid slot is taken (`FUN_1404de8c0` walls / `FUN_1404de610` floors / `FUN_1404de3e0` centre cells; the
//!   occupant is only replaced when it has `bDestroyOnPlayerBuildingPlacement`, which no player piece has).
//! * Obstructed (2): the box sweep `FUN_1404df7e0` ("WorldOverlapCheck", `crate::structural::world_overlap_check` with allow = 0) hits
//!   a blocking building actor or a `StaticMeshActor` above the base. Flags (+0x564: bit0 `bDoNotBlockBuildings`, bit1
//!   `bDestroyOnPlayerBuildingPlacement`, bit2 `bUseCentroidForBlockBuildingsCheck`): `ABuildingActor` ctor sets `|= 5`, but the
//!   `ABuildingSMActor` ctor @14045b960 then does `&= ~3; |= 4`, so every SM building actor BLOCKS with the centroid test;
//!   `ABuildingProp` ctor @14045adb0 and `ABuildingContainer` clear bit2 (props / containers block with no centroid test; the cooked
//!   `Athena_Tree_*` -> `Parent_Tree` and `Athena_Prop_Rocks_*` -> `BuildingProp` do not override the bits), `ABuildingPropCorner` sets
//!   bit0 (never blocks). So trees, rocks and containers ([`PlacementEnv::props`]) obstruct any piece whose sweep box touches them,
//!   and a static mesh actor ([`PlacementEnv::static_meshes`]) obstructs when the hit is above the base. Placed player pieces never
//!   obstruct each other: because of the centroid test (no neighbour's centroid lies inside these thin boxes above the base), not
//!   because of the flag; the port therefore leaves them out of the sweep (the slot check above handles the same slot).
//!   A prop hit never counts as ground. Cooked classes override the bits ([`BlockFlags`]): every bush (`Plant_Shrub_*`, a
//!   BuildingContainer), hedge, small rock / small tree, crate and piece of furniture has `bDestroyOnPlayerBuildingPlacement`: it never
//!   obstructs, and a placement destroys it ([`placement_destroys`]).
//! * NoStructuralSupport (3): the same sweep did not touch the ground (landscape, or a static mesh actor at/below the base) AND no
//!   neighbouring piece connects (`FUN_1405047a0`: the cell-map neighbours of `FUN_1404ee8c0` (`crate::structural::gather`) that are
//!   themselves structurally supported, then `FUN_1404daa90`, the connectivity-cube lattice test below).
//!
//! ## Connectivity (`FUN_1404daa90` -> `FUN_1404f75f0` / `FUN_1404f8bb0` / `FUN_1404f92d0`)
//! Each piece's edit-pattern cube (6 faces x 5x5 bits) is placed in its structural cell (walls: on the cell face of their side; mirrored
//! when `bMirrored` and again for yaw quadrant 1 / 2; floors / centre cells rotated round(yaw / 90) times, odd counts +2). Every set bit is a
//! point of a lattice with 4 points per cell horizontally and 3 vertically (128 cm). Two pieces connect when they share MORE THAN ONE
//! lattice point (an edge, not a corner).
use glam::{IVec3, Vec3};

use crate::grid::{self, cell_of, on_level, wall_side, Rot, Slot};
use crate::pieces::{ConnectivityCube, Material, PieceCatalog, PieceDef, PieceKind};
use crate::world::{BuildingWorld, PieceId, PieceSpawn, PlacedPiece};

/// `EFortStructuralGridQueryResults` (the values the port can produce).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryResult {
    CanAdd,
    ExistingActor(PieceId),
    Obstructed,
    NoStructuralSupport,
    /// No class / no catalog entry (`InvalidActor`, 4).
    InvalidActor,
}
impl QueryResult {
    pub fn code(self) -> u8 { match self { QueryResult::CanAdd => 0, QueryResult::ExistingActor(_) => 1, QueryResult::Obstructed => 2, QueryResult::NoStructuralSupport => 3, QueryResult::InvalidActor => 4 } }
}

/// What the world outside the building system contributes to placement.
#[derive(Clone, Debug, Default)]
pub struct PlacementEnv {
    /// Height (level-space Z, cm) of the flat ground plane the port collides with (the baseplate). The 1.8 landscape gives support
    /// to any piece whose "WorldOverlapCheck" sweep crosses it. `None` = no ground.
    pub ground_z: Option<f32>,
    /// TERRAIN: the landscape in world cm (`fn_terrain::Local` with the movement scene's z offset). When set it is the ground of the "WorldOverlapCheck"
    /// ([`crate::structural::Ground`]: `Landscape` hits); `ground_z` then only stands in under holes of it.
    pub terrain: Option<fn_terrain::Local>,
    /// Where the level origin (0, 0, 0) of the Athena map lies in the world space: the grid snaps and cell indices are taken relative to
    /// it, so the lattice is the level's (the exe snaps absolute level coordinates). 0 when the port has no level mapping.
    pub grid_origin: Vec3,
    /// The live `BuildingProp` / `BuildingContainer` actors near the player (trees, rocks, bushes, hedges, chests, ...): their collision
    /// (world cm convexes) and +0x564 flags. With the native defaults (no centroid test, blocking) an overlap with a piece's sweep box
    /// makes the query Obstructed; the many cooked classes with `bDestroyOnPlayerBuildingPlacement` (every bush and hedge, small rocks,
    /// small trees, furniture, crates) do not block and are destroyed by the placement instead ([`placement_destroys`]). A prop hit
    /// never gives ground support.
    pub props: std::sync::Arc<Vec<PropCollider>>,
    /// HOOK (G3): collision of non-building `AStaticMeshActor`s (world cm). In the "WorldOverlapCheck" sweep a hit above the base
    /// obstructs and one at / below it is ground; in `BuildingPlaceTrace` they are accepted targets (a hit through a
    /// `UStaticMeshComponent` of a non-building actor). Empty while the port has no such actors. Pre-placed POI walls / floors are
    /// `ABuildingSMActor`s with the centroid rule like player pieces: they belong in the `BuildingWorld` (slot checks, targeting).
    pub static_meshes: std::sync::Arc<Vec<fn_hull::Hull>>,
    /// Structural grid bounds relative to `grid_origin` (min, max, world cm), `FUN_1404f2160` (from `AFortWorldManager` vfunc_188):
    /// origin Z = -3840 (0xC5700000), X / Y whole multiples of 512 from the world manager's `X_Size`, `Y_Size`, `TileEdgeSize`,
    /// height max(20, `Z_StructuralGrid_Size`) levels of 384. A piece outside the box gives InvalidActor (4). `None` = unbounded.
    pub grid_bounds: Option<(Vec3, Vec3)>,
    pub poi_pieces: std::sync::Arc<Vec<PoiPiece>>,
}

pub const POI_PIECE_ID: PieceId = PieceId::MAX;

#[derive(Clone, Debug)]
pub struct PoiPiece {
    pub id: u64,
    pub kind: PieceKind,
    pub location: Vec3,
    pub yaw: f32,
    pub slot: Slot,
    pub voxel: Voxel,
    pub hulls: Vec<fn_hull::Hull>,
}
impl PoiPiece {
    pub fn new(id: u64, kind: PieceKind, location: Vec3, yaw: f32, grid_origin: Vec3, hulls: Vec<fn_hull::Hull>) -> Self {
        let rel = location - grid_origin;
        let slot = grid::key_for(kind, rel, yaw).1;
        let voxel = voxel(kind, crate::structural::base_cube(kind), rel, yaw, false);
        PoiPiece { id, kind, location, yaw, slot, voxel, hulls }
    }
}

pub fn anchored(env: &PlacementEnv, v: &Voxel) -> bool {
    env.poi_pieces.iter().any(|p| (p.voxel.cell - v.cell).abs().max_element() <= 2 && connected(v, &p.voxel))
}

/// The lower Z edge of the structural grid relative to the level origin (`FUN_1404f2160` stores 0xC5700000 = -3840 = -10 x 384 at +0x3CC).
pub const STRUCTURAL_GRID_MIN_Z: f32 = -3840.;
/// The grid has at least 20 levels (`FUN_1404f2160`: levels = max(20, param_6)); native `AFortWorldManager` ctor @14062ce50 default
/// `Z_StructuralGrid_Size` = 50 (+0x440), `X_Size` / `Y_Size` = 10, `TileEdgeSize` = 25 (an Athena override, if any, is not known).
pub const STRUCTURAL_GRID_MIN_LEVELS: i32 = 20;

/// `ABuildingActor` +0x564 bits read by the "WorldOverlapCheck" (`FUN_1404df7e0`): bit0 `bDoNotBlockBuildings`, bit1
/// `bDestroyOnPlayerBuildingPlacement`, bit2 `bUseCentroidForBlockBuildingsCheck`. Native defaults: `ABuildingSMActor` ctor @14045b960
/// `&= ~3; |= 4` (blocks, centroid test), `ABuildingProp` ctor @14045adb0 and `ABuildingContainer` ctor @140458c60 clear bit2 (blocks,
/// no centroid test); the cooked class defaults override them per class (e.g. every `Plant_Shrub_*` and `Hedge_*`:
/// `bDestroyOnPlayerBuildingPlacement` true).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockFlags { pub do_not_block_buildings: bool, pub destroy_on_player_building_placement: bool, pub use_centroid: bool }

pub const PROP_STATIC_MESH: u8 = 8;
pub const PROP_NOT_IN_SWEEP: u8 = 16;

/// One live building actor of the world (not a player piece) as the placement sweep sees it.
#[derive(Clone, Debug)]
pub struct PropCollider {
    /// The caller's handle for the actor (returned by [`placement_destroys`]).
    pub id: u64,
    /// Collision convexes, world cm.
    pub hulls: Vec<fn_hull::Hull>,
    pub flags: BlockFlags,
    /// `ABuildingActor::GetCentroid` (vtable +0x7F0), world cm: only read when `flags.use_centroid`.
    pub centroid: Vec3,
}
impl PropCollider {
    /// A prop with the native `ABuildingProp` defaults (blocks, no centroid test).
    pub fn blocking(id: u64, hulls: Vec<fn_hull::Hull>) -> Self {
        let centroid = hulls.iter().map(|h| h.center).sum::<Vec3>() / (hulls.len().max(1) as f32);
        PropCollider { id, hulls, flags: BlockFlags::default(), centroid }
    }
}

/// The "WorldOverlapCheck" world of a placement query: the ground plane, the props and the static mesh actors of a [`PlacementEnv`].
pub struct PlacementCollision<'a> { pub env: &'a PlacementEnv }
impl crate::structural::WorldCollision for PlacementCollision<'_> {
    fn sweep_box(&self, start: Vec3, end: Vec3, half: Vec3) -> Vec<crate::structural::WorldHit> {
        use crate::structural::{Ground, WorldHit, WorldHitKind};
        let mut hits = Ground { terrain: self.env.terrain.clone(), flat_z: self.env.ground_z }.sweep_box(start, end, half);
        let (lo, hi) = (start.min(end) - half, start.max(end) + half);
        // building actors (props, containers): one hit per actor whose collision the swept box touches, with its own +0x564 flags
        for p in self.env.props.iter() {
            if p.hulls.iter().any(|h| box_overlaps_hull(lo, hi, h)) {
                let f = p.flags;
                hits.push(WorldHit { kind: WorldHitKind::BuildingActor { do_not_block_buildings: f.do_not_block_buildings, destroy_on_player_building_placement: f.destroy_on_player_building_placement, use_centroid: f.use_centroid, centroid: p.centroid }, location: start, actor: Some(p.id) });
            }
        }
        for h in self.env.static_meshes.iter().filter(|h| h.max.x >= lo.x && h.min.x <= hi.x && h.max.y >= lo.y && h.min.y <= hi.y && h.max.z >= lo.z && h.min.z <= hi.z) {
            if let Some(z) = first_contact_z(start, end, half, h) { hits.push(WorldHit { kind: WorldHitKind::StaticMeshActor, location: Vec3::new(start.x, start.y, z), actor: None }); }
        }
        hits
    }
}

/// The actors a placement of a `kind` piece at `loc` / `yaw` destroys: `AFortPlayerController::ServerCreateBuildingActor` @140845d70,
/// after a query of 0 (CanAdd), kills every actor the "WorldOverlapCheck" listed that has `bDestroyOnPlayerBuildingPlacement`
/// (ABuildingActor vfunc_243 @140468b50 with the placement death tag). Ids of [`PlacementEnv::props`].
pub fn placement_destroys(env: &PlacementEnv, kind: PieceKind, loc: Vec3, yaw: f32) -> Vec<u64> { sweep_listing(env, kind, loc, yaw).1 }

/// The building actors the "WorldOverlapCheck" of a placement lists, split as the build preview update (FUN_140835750) splits them for its highlight
/// (`AFortPlayerController` vfunc_672): (blocking = +0x564 & 3 == 0, the ones that make the query Obstructed; destroyed = `bDestroyOnPlayerBuildingPlacement`).
pub fn sweep_listing(env: &PlacementEnv, kind: PieceKind, loc: Vec3, yaw: f32) -> (Vec<u64>, Vec<u64>) {
    use crate::structural::{world_overlap_check, overlap_box, WorldCollision, WorldHitKind};
    let o = env.grid_origin;
    let rel = grid::grid_location(kind, loc - o, yaw);
    let side = if kind == PieceKind::Wall { wall_side(rel, cell_of(rel)) } else { 0 };
    let b = overlap_box(kind, rel + o, side);
    let coll = PlacementCollision { env };
    let hits = coll.sweep_box(b.start, b.end, b.half_extent);
    let r = world_overlap_check(&coll, &b, false);
    let (mut blocking, mut destroyed) = (vec![], vec![]);
    for h in r.building_hits.iter().filter_map(|&i| hits.get(i)) {
        let (WorldHitKind::BuildingActor { destroy_on_player_building_placement, .. }, Some(a)) = (h.kind, h.actor) else { continue };
        if destroy_on_player_building_placement { destroyed.push(a) } else { blocking.push(a) }
    }
    (blocking, destroyed)
}

/// Separating-axis test of the axis-aligned box `[lo, hi]` against a convex hull (touching counts as overlap).
pub fn box_overlaps_hull(lo: Vec3, hi: Vec3, h: &fn_hull::Hull) -> bool {
    if h.verts.is_empty() || h.max.x < lo.x || h.min.x > hi.x || h.max.y < lo.y || h.min.y > hi.y || h.max.z < lo.z || h.min.z > hi.z { return false; }
    let (c, e) = ((lo + hi) * 0.5, (hi - lo) * 0.5);
    let separated = |a: Vec3| {
        let r = e.x * a.x.abs() + e.y * a.y.abs() + e.z * a.z.abs();
        let (mut mn, mut mx) = (f32::MAX, f32::MIN);
        for v in &h.verts { let p = a.dot(*v); mn = mn.min(p); mx = mx.max(p); }
        let cp = a.dot(c);
        mn > cp + r || mx < cp - r
    };
    if h.planes.iter().any(|(n, _)| separated(*n)) { return false; }
    for ed in h.edges.iter() {
        let d = h.verts[ed[1] as usize] - h.verts[ed[0] as usize];
        for ax in [Vec3::X, Vec3::Y, Vec3::Z] { let a = ax.cross(d); if a.length_squared() > 1e-12 && separated(a) { return false; } }
    }
    true
}

/// Box centre Z of the first contact of a box of `half` swept from `start` down to `end` (vertical sweep) with a hull (`FHitResult::Location`).
fn first_contact_z(start: Vec3, end: Vec3, half: Vec3, h: &fn_hull::Hull) -> Option<f32> {
    let swept = |z: f32| box_overlaps_hull(Vec3::new(start.x, start.y, z.min(start.z)) - half, Vec3::new(start.x, start.y, start.z.max(z)) + half, h);
    if !swept(end.z) { return None; }
    if box_overlaps_hull(start - half, start + half, h) { return Some(start.z); }
    let (mut above, mut below) = (start.z, end.z); // no contact over [above, start], contact over [below, start]
    for _ in 0..32 { let m = 0.5 * (above + below); if swept(m) { below = m } else { above = m } }
    Some(below)
}

// ---------------------------------------------------------------------------------------------------------------------------
// Connectivity cubes
// ---------------------------------------------------------------------------------------------------------------------------

fn bit(c: &ConnectivityCube, face: usize, row: i32, col: i32) -> bool { (c[face] >> (row * 5 + col)) & 1 != 0 }

/// `FUN_1404c34c0`: 90° turn of a 5x5 top / bottom face, new(r, c) = old(c, 4 - r).
fn rotate_face(w: u32) -> u32 {
    let mut o = 0u32;
    for r in 0..5 { for c in 0..5 { if (w >> (c * 5 + (4 - r))) & 1 != 0 { o |= 1 << (r * 5 + c); } } }
    o
}
/// `FUN_1404bb670`: columns mirrored (col <-> 4 - col).
fn flip_face(w: u32) -> u32 {
    let mut o = 0u32;
    for r in 0..5 { for c in 0..5 { if (w >> (r * 5 + c)) & 1 != 0 { o |= 1 << (r * 5 + (4 - c)); } } }
    o
}
/// `FUN_1404c3140`: one yaw quarter turn of the cube (faces 4 / 5 turned, side faces cycled 2 <- 3 <- 0 <- 1 <- 2).
pub fn rotate_cube(c: ConnectivityCube) -> ConnectivityCube { [c[1], c[2], c[3], c[0], rotate_face(c[4]), rotate_face(c[5])] }
/// `FUN_1404bb3d0`: mirror (faces 0, 2, 4, 5 flipped; 1 and 3 swapped, then flipped).
pub fn mirror_cube(c: ConnectivityCube) -> ConnectivityCube { [flip_face(c[0]), flip_face(c[3]), flip_face(c[2]), flip_face(c[1]), flip_face(c[4]), flip_face(c[5])] }

/// Yaw -> [0, 360) as the exe computes it before the quadrant (fmod by 360, negative + 360).
fn yaw_0_360(yaw: f32) -> f32 { let mut y = yaw % 360.; if y < 0. { y += 360.; } y }
fn quadrant(yaw: f32) -> i32 { (yaw_0_360(yaw) / 90. + 0.5).floor() as i32 }

/// A piece placed in the structural lattice: its (adjusted) cell and oriented cube (`FUN_1404f75f0`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Voxel { pub cell: IVec3, pub cube: ConnectivityCube }

/// `loc` relative to the grid origin.
pub fn voxel(kind: PieceKind, cube: ConnectivityCube, loc: Vec3, yaw: f32, mirrored: bool) -> Voxel {
    let gl = grid::grid_location(kind, loc, yaw);
    let mut cell = cell_of(gl);
    let mut c = cube;
    match kind {
        PieceKind::Wall => {
            // FUN_1404f7b80
            if mirrored { c = mirror_cube(c); }
            if ((quadrant(yaw) - 1) as u32) < 2 { c = mirror_cube(c); } // yaw quadrant 1 or 2
            let side = wall_side(gl, cell);
            if side == 3 { cell.y -= 1; } else if side < 2 { c = rotate_cube(c); if side == 0 { cell.x -= 1; } }
        }
        _ => {
            // FUN_1404f79d0 (floors: + the level adjust) / FUN_1404f7840 (centre cells)
            if kind == PieceKind::Floor && !on_level(gl, cell) { cell.z += 1; }
            if mirrored { c = mirror_cube(c); }
            let mut q = quadrant(yaw);
            if q & 1 != 0 { q = (q + 2) & 3; }
            for _ in 0..q { c = rotate_cube(c); }
        }
    }
    Voxel { cell, cube: c }
}

/// Lattice points of the set bits of a cube placed at `cell` (`FUN_1404f8bb0` face table, `FUN_1404f92d0` walk).
fn lattice_points(v: &Voxel, cell: IVec3, out: &mut Vec<IVec3>) {
    let (x, y, z) = (cell.x * 4, cell.y * 4, cell.z * 3);
    // (face, start, column step, row step, rows)
    let faces: [(usize, IVec3, IVec3, IVec3, i32); 6] = [
        (1, IVec3::new(x, y, z + 3), IVec3::Y, IVec3::NEG_Z, 4),
        (0, IVec3::new(x, y + 4, z + 3), IVec3::X, IVec3::NEG_Z, 4),
        (3, IVec3::new(x + 4, y + 4, z + 3), IVec3::NEG_Y, IVec3::NEG_Z, 4),
        (2, IVec3::new(x + 4, y, z + 3), IVec3::NEG_X, IVec3::NEG_Z, 4),
        (5, IVec3::new(x, y, z), IVec3::X, IVec3::Y, 5),
        (4, IVec3::new(x, y, z + 3), IVec3::X, IVec3::Y, 5),
    ];
    for (f, start, col, row, rows) in faces {
        for r in 0..rows { for c in 0..5 { if bit(&v.cube, f, r, c) { out.push(start + row * r + col * c); } } }
    }
}

/// `FUN_1404daa90`: true when the two placed cubes share more than one lattice point (`FUN_1404f8bb0` returns the number of DISTINCT points
/// of B found in A's set, the test is `1 < count`). A is inserted at d = (1,1,1), B at `cellB - cellA + (1,1,1)`; `FUN_1404f8bb0` skips any
/// d with a component of magnitude >= 3.
/// STRUCTURAL FIX (2026-10-05): A's points (all within lattice x, y 4..=8, z 3..=6 at d = (1,1,1)) are kept in a fixed 5x5x4 table
/// instead of a Vec, so the structural system and the placement query can run the test per gathered neighbour cheaply.
pub fn connected(a: &Voxel, b: &Voxel) -> bool {
    let off = b.cell - a.cell + IVec3::ONE;
    if off.x.abs() >= 3 || off.y.abs() >= 3 || off.z.abs() >= 3 { return false; }
    let slot = |p: IVec3| -> Option<usize> {
        let q = p - IVec3::new(4, 4, 3);
        if q.x < 0 || q.y < 0 || q.z < 0 || q.x > 4 || q.y > 4 || q.z > 3 { return None; }
        Some((q.x * 20 + q.y * 4 + q.z) as usize)
    };
    let mut in_a = [false; 100];
    let mut pa = Vec::with_capacity(64);
    lattice_points(a, IVec3::ONE, &mut pa);
    for p in pa { if let Some(i) = slot(p) { in_a[i] = true; } }
    let mut pb = Vec::with_capacity(64);
    lattice_points(b, off, &mut pb);
    let mut counted = [false; 100];
    let mut n = 0;
    for p in pb { if let Some(i) = slot(p) { if in_a[i] && !counted[i] { counted[i] = true; n += 1; if n > 1 { return true; } } } }
    false
}

// ---------------------------------------------------------------------------------------------------------------------------
// Structural query
// ---------------------------------------------------------------------------------------------------------------------------

/// Occupancy slot of a placed piece (its class geometry is the piece kind's; edit variants keep the slot).
pub fn slot_of(p: &PlacedPiece, origin: Vec3) -> Slot { grid::key_for(p.kind, p.location - origin, p.yaw_deg).1 }

/// The cube of a placed piece (its current class: an edit variant has its own pattern).
fn piece_voxel(cat: &PieceCatalog, p: &PlacedPiece, origin: Vec3) -> Option<Voxel> {
    let cls = cat.class(p.effective_class())?;
    Some(voxel(p.kind, cls.cube, p.location - origin, p.yaw_deg, p.edit.mirrored))
}

/// The "WorldOverlapCheck" sweep span of a piece above / below its grid location (`FUN_1404de8c0` walls: +375 / -18, `FUN_1404de610`
/// floors: +15 / -18, `FUN_1404de3e0` centre cells: +375 / -18; DAT_1440c5e40 = 375, DAT_144040c98 = 15, DAT_1440c5e30 = 18).
pub fn sweep_span(kind: PieceKind) -> (f32, f32) { match kind { PieceKind::Floor => (15., 18.), _ => (375., 18.) } }

/// `FUN_1404ddf50` for a candidate transform.
pub fn query(world: &BuildingWorld, cat: &PieceCatalog, def: &PieceDef, loc: Vec3, yaw: f32, mirrored: bool, env: &PlacementEnv) -> QueryResult {
    let kind = def.kind;
    let o = env.grid_origin;
    let rel = grid::grid_location(kind, loc - o, yaw);
    // structural grid bounds (FUN_1404f2160; outside -> InvalidActor 4, docs/spec/building_core_gaps.md G5)
    if let Some((mn, mx)) = env.grid_bounds { if rel.cmplt(mn).any() || rel.cmpgt(mx).any() { return QueryResult::InvalidActor; } }
    let (_, slot) = grid::key_for(kind, loc - o, yaw);
    if let Some(p) = world.iter().find(|p| slot_of(p, o) == slot) { return QueryResult::ExistingActor(p.id); }
    if env.poi_pieces.iter().any(|p| p.slot == slot) { return QueryResult::ExistingActor(POI_PIECE_ID); }
    // "WorldOverlapCheck" (FUN_1404df7e0, allow = 0) of the slot's box (crate::structural::overlap_box: walls / centre cells
    // gl.z + 375 .. gl.z - 18, floors gl.z + 15 .. gl.z - 18): a blocking prop / container or a static mesh actor above the base
    // -> Obstructed; the landscape (baseplate) or a static mesh actor at / below the base -> supported.
    let side = if kind == PieceKind::Wall { wall_side(rel, cell_of(rel)) } else { 0 };
    let sweep = crate::structural::world_overlap_check(&PlacementCollision { env }, &crate::structural::overlap_box(kind, rel + o, side), false);
    if !sweep.ok && obstruction_enabled() { return QueryResult::Obstructed; }
    if sweep.touched_world { return QueryResult::CanAdd; }
    // neighbour connection (FUN_1405047a0).
    // STRUCTURAL FIX (2026-10-05): the candidates are the cell-map neighbours of FUN_1404ee8c0 (`structural::gather`, walls / floors / centre
    // cells by type, wall keys FUN_1404f0160) instead of every piece within 2 cells, and (authority: Role +0x118 == 3) a candidate counts
    // only when it is itself structurally supported (IsBuildingActorStructurallySupported @1404f3dc0) before the cube test FUN_1404daa90.
    // Its support is its connected component holding a piece whose "WorldOverlapCheck" touched the ground (`structural::component_supported`).
    let me = voxel(kind, def.class.cube, loc - o, yaw, mirrored);
    let idx = crate::structural::CellIndex::from_world(world, o);
    let (_, nb) = crate::structural::gather(&idx, kind, grid::grid_location(kind, loc - o, yaw));
    // STRUCTURAL FIX 2 (2026-10-05): the neighbours' world support from the same WorldCollision as the candidate's sweep (allow = 1 ignores
    // the props, a static mesh actor counts as world), not a separate ground height (`structural::FlatGround` documents the baseplate limit)
    let ground = PlacementCollision { env };
    let voxel_of = |p: &PlacedPiece| piece_voxel(cat, p, o);
    let world_supported = |p: &PlacedPiece| {
        let rel = grid::grid_location(p.kind, p.location - o, p.yaw_deg);
        let side = if p.kind == PieceKind::Wall { wall_side(rel, cell_of(rel)) } else { 0 };
        // the register functions sweep with allow = 1 (FUN_1404d9250 / d9d80 / d90f0)
        crate::structural::world_overlap_check(&ground, &crate::structural::overlap_box(p.kind, rel + o, side), true).touched_world || voxel_of(p).is_some_and(|v| anchored(env, &v))
    };
    for (n, _) in nb.all() {
        let Some(p) = world.get(n) else { continue };
        let Some(v) = voxel_of(p) else { continue };
        if !connected(&me, &v) { continue; }
        if crate::structural::component_supported(world, &idx, o, n, &voxel_of, &world_supported) { return QueryResult::CanAdd; }
    }
    if anchored(env, &me) { return QueryResult::CanAdd; }
    QueryResult::NoStructuralSupport
}

// ---------------------------------------------------------------------------------------------------------------------------
// Collision of placed pieces
// ---------------------------------------------------------------------------------------------------------------------------

/// World-space convex hulls (cm) of a placed piece: the class's cooked collision convexes at the actor transform (mirror = local X * -1).
pub fn piece_hulls(cat: &PieceCatalog, p: &PlacedPiece) -> Vec<fn_hull::Hull> {
    let Some(cls) = cat.class(p.effective_class()) else { return vec![] };
    transform_hulls(&cls.convexes, p.location, p.yaw_deg, p.edit.mirrored)
}
pub fn transform_hulls(convexes: &[Vec<Vec3>], loc: Vec3, yaw: f32, mirrored: bool) -> Vec<fn_hull::Hull> {
    convexes.iter().filter_map(|c| {
        let pts: Vec<Vec3> = c.iter().map(|v| { let v = if mirrored { Vec3::new(-v.x, v.y, v.z) } else { *v }; loc + grid::rotate_yaw(v, yaw) }).collect();
        fn_hull::Hull::from_points(&pts)
    }).collect()
}

/// Line trace against the placed pieces (`BuildingPlaceTrace`): first entering hit (t, impact point, normal), start-inside hulls skipped.
pub fn trace_pieces(hulls: &[fn_hull::Hull], from: Vec3, to: Vec3) -> Option<(f32, Vec3, Vec3)> {
    let d = to - from; let len = d.length();
    if len < 1e-6 { return None; }
    let dir = d / len;
    let mut best: Option<(f32, Vec3, Vec3)> = None;
    for h in hulls {
        if h.sdf(from) < 0. { continue; } // bStartPenetrating hits are skipped
        if let Some((t, n)) = h.ray(from, dir, len) { if best.map_or(true, |b| t < b.0) { best = Some((t, from + dir * t, n)); } }
    }
    best
}

/// `FMath::LineExtentBoxIntersection` with zero extent: where the segment `start -> end` enters `[min, max]`.
fn line_box_entry(min: Vec3, max: Vec3, start: Vec3, end: Vec3) -> Option<Vec3> {
    let d = end - start;
    let (mut t0, mut t1) = (0f32, 1f32);
    for i in 0..3 {
        if d[i].abs() < 1e-12 { if start[i] < min[i] || start[i] > max[i] { return None; } continue; }
        let (mut a, mut b) = ((min[i] - start[i]) / d[i], (max[i] - start[i]) / d[i]);
        if a > b { std::mem::swap(&mut a, &mut b); }
        t0 = t0.max(a); t1 = t1.min(b);
        if t0 > t1 { return None; }
    }
    Some(start + d * t0)
}

// ---------------------------------------------------------------------------------------------------------------------------
// The build preview (AFortPlayerController building state)
// ---------------------------------------------------------------------------------------------------------------------------

/// Camera / pawn inputs of one preview update (level space, cm / degrees).
#[derive(Clone, Copy, Debug)]
pub struct ViewInput {
    pub camera: Vec3,
    pub view: Rot,
    /// Pawn root location (capsule centre).
    pub pawn: Vec3,
}

/// The building preview state of one player (`BuildPreviewMarker` transform, `BuildPreviewRotationIterations`, `bBuildPreviewMirrored`,
/// `LastBuildPreviewGridSnapLoc/Rot/Centroid`, the current buildable).
#[derive(Clone, Debug)]
pub struct BuildPreview {
    pub kind: PieceKind,
    pub material: Material,
    pub rotation_iterations: u8,
    pub mirrored: bool,
    /// Preview actor transform (what is drawn) and the class geometry it was given (`ABuildingSMActor` vfunc_252 copies the class offsets).
    pub loc: Vec3,
    pub rot: Rot,
    marker_kind: PieceKind,
    pub last_loc: Vec3,
    pub last_rot: Rot,
    pub last_centroid: Vec3,
    /// Query result of the current preview transform.
    pub result: QueryResult,
    pub active: bool,
    pub snapped: bool,
    saved_iterations: [u8; 4],
}

impl Default for BuildPreview {
    fn default() -> Self {
        BuildPreview { kind: PieceKind::Wall, material: Material::Wood, rotation_iterations: 0, mirrored: false, loc: Vec3::ZERO, rot: Rot::default(), marker_kind: PieceKind::Wall,
            last_loc: Vec3::ZERO, last_rot: Rot::default(), last_centroid: Vec3::ZERO, result: QueryResult::InvalidActor, active: false, snapped: false, saved_iterations: [0; 4] }
    }
}

fn kind_index(kind: PieceKind) -> usize { match kind { PieceKind::Wall => 0, PieceKind::Floor => 1, PieceKind::Stairs => 2, PieceKind::Roof => 3 } }

impl BuildPreview {
    /// Selects the buildable (the preview marker takes the class geometry; its transform stays until the next update).
    pub fn set_piece(&mut self, kind: PieceKind) {
        if kind != self.kind { self.rotation_iterations = self.saved_iterations[kind_index(kind)]; }
        self.kind = kind;
    }

    pub fn rotate(&mut self, forward: bool) {
        let it = if self.kind == PieceKind::Wall { if self.rotation_iterations == 0 { 2 } else { 0 } }
            else if forward { self.rotation_iterations.wrapping_add(1) & 3 } else { self.rotation_iterations.wrapping_sub(1) & 3 };
        self.rotation_iterations = it;
        self.saved_iterations[kind_index(self.kind)] = it;
    }

    pub fn prepare_place(&mut self) {
        if self.kind == PieceKind::Wall && (self.rotation_iterations.wrapping_sub(1) & 0xfd) == 0 { self.rotation_iterations = 0; }
    }

    /// One preview update (@140835750 + vfunc_688). Returns the query result of the new preview transform.
    pub fn update(&mut self, world: &BuildingWorld, cat: &PieceCatalog, env: &PlacementEnv, v: &ViewInput) -> QueryResult {
        let Some(def) = cat.def(self.kind, self.material) else { self.result = QueryResult::InvalidActor; return self.result };
        // the marker now carries the selected class's offsets; its centroid at the old transform is what vfunc_688 compares
        self.marker_kind = self.kind;
        let marker_centroid = grid::centroid(self.marker_kind, self.loc, self.rot.yaw);
        let (loc, rot) = self.target(world, cat, def, env, v, marker_centroid);
        self.result = query(world, cat, def, loc, rot.yaw, self.mirrored, env);
        self.loc = loc; self.rot = rot; self.active = true;
        let moved = (loc - self.last_loc).abs().max_element() > grid::KINDA_SMALL
            || [rot.pitch - self.last_rot.pitch, rot.yaw - self.last_rot.yaw, rot.roll - self.last_rot.roll].iter().any(|d| { let m = d.rem_euclid(360.); m > 1e-4 && 360. - m > 1e-4 });
        if moved { self.last_loc = loc; self.last_rot = rot; self.last_centroid = grid::centroid(self.kind, loc, rot.yaw); }
        self.snapped = moved;
        self.result
    }

    /// vfunc_688 @140834290.
    fn target(&mut self, world: &BuildingWorld, cat: &PieceCatalog, def: &PieceDef, env: &PlacementEnv, v: &ViewInput, marker_centroid: Vec3) -> (Vec3, Rot) {
        let kind = def.kind;
        let bpd = grid::geom(kind).placement_distance;
        let dir = v.view.vector();
        let d = (v.pawn - v.camera).length();
        let far = d + bpd.length();
        let p0 = v.camera + dir * d;
        let mut p1 = v.camera + dir * far;
        let (bmin, bmax) = (p0 - bpd, p0 + bpd);
        let inside = p1.x > bmin.x && p1.x < bmax.x && p1.y > bmin.y && p1.y < bmax.y && p1.z > bmin.z && p1.z < bmax.z;
        if !inside { if let Some(h) = line_box_entry(bmin, bmax, p1, p0) { p1 = h; } }
        let mut target = p1;
        // BuildingPlaceTrace against the placed pieces (+ HOOK G3: non-building static mesh actors are accepted targets too; POI
        // building actors are accepted through the world; props, containers and the landscape never stop the trace)
        let (tlo, thi) = (p0.min(p1), p0.max(p1));
        let hulls: Vec<fn_hull::Hull> = world.iter().flat_map(|p| piece_hulls(cat, p)).chain(env.static_meshes.iter().filter(|h| h.max.x >= tlo.x && h.min.x <= thi.x && h.max.y >= tlo.y && h.min.y <= thi.y && h.max.z >= tlo.z && h.min.z <= thi.z).cloned()).chain(env.poi_pieces.iter().flat_map(|p| p.hulls.iter()).filter(|h| h.max.x >= tlo.x && h.min.x <= thi.x && h.max.y >= tlo.y && h.min.y <= thi.y && h.max.z >= tlo.z && h.min.z <= thi.z).cloned()).collect();
        if let Some((_, at, n)) = trace_pieces(&hulls, p0, p1) { target = at + n * grid::TRACE_NORMAL_PUSH; }

        let o = env.grid_origin;
        let snap = |iter: u8| { let (l, r) = grid::snap_transform(kind, target - o, v.view, iter); (l + o, r) };
        let (loc, rot) = snap(self.rotation_iterations);
        let r = query(world, cat, def, loc, rot.yaw, self.mirrored, env);
        if r != QueryResult::NoStructuralSupport { return (loc, rot); }
        if (marker_centroid - self.last_centroid).abs().max_element() > grid::KINDA_SMALL {
            let dyaw = grid::normalize_axis((v.view.yaw - rot.yaw) + grid::YAW_STEP);
            let iter = if dyaw >= 0. { self.rotation_iterations.wrapping_add(1) } else { self.rotation_iterations.wrapping_add(3) };
            let (l2, r2) = snap(iter);
            match query(world, cat, def, l2, r2.yaw, self.mirrored, env) {
                QueryResult::CanAdd => { self.rotation_iterations = iter; return (l2, r2); }
                QueryResult::NoStructuralSupport => {}
                _ => return (loc, rot),
            }
        }
        // one level toward the view's vertical direction, then the other way (fVar10 = far * Dir.Z, DAT_143fe36fc = 1, DAT_143feb1ec = -1)
        let vz = far * dir.z;
        let sign = if vz > 0. { 1. } else if vz < 0. { -1. } else { 0. };
        for s in [sign, -sign] {
            let l = Vec3::new(loc.x, loc.y, loc.z + grid::VERT_SNAP_GRID_SIZE * s);
            if query(world, cat, def, l, rot.yaw, self.mirrored, env) == QueryResult::CanAdd { return (l, rot); }
        }
        (loc, rot)
    }

    /// The transform a placement would use (`ServerCreateBuildingActor(.., LastBuildPreviewGridSnapLoc, LastBuildPreviewGridSnapRot, bMirrored)`).
    pub fn placement(&self) -> (Vec3, Rot) { (self.last_loc, self.last_rot) }
}

/// Places the previewed piece when the query allows it and `available` covers the cost; returns the new id and the spent amount.
pub fn place(world: &mut BuildingWorld, cat: &PieceCatalog, env: &PlacementEnv, preview: &BuildPreview, available: u32) -> Result<(PieceId, u32), QueryResult> {
    let def = cat.def(preview.kind, preview.material).ok_or(QueryResult::InvalidActor)?;
    let (loc, rot) = (preview.loc, preview.rot);
    let r = query(world, cat, def, loc, rot.yaw, preview.mirrored, env);
    if r != QueryResult::CanAdd { return Err(r); }
    if available < def.cost { return Err(QueryResult::InvalidActor); }
    Ok((spawn_piece(world, def, loc, rot.yaw, preview.mirrored, env.grid_origin), def.cost))
}

/// Spawns a piece of `def` at a transform (no validation): health MaxHealth * InitialHealthPct, build time from the class attributes.
pub fn spawn_piece(world: &mut BuildingWorld, def: &PieceDef, loc: Vec3, yaw: f32, mirrored: bool, grid_origin: Vec3) -> PieceId {
    let (key, _) = grid::key_for(def.kind, loc - grid_origin, yaw);
    let id = world.spawn(PieceSpawn { kind: def.kind, material: def.material, class_path: def.class.class_path.clone(), key, location: loc, yaw_deg: yaw, max_health: def.max_health, start_health: def.start_health(), build_time: def.build_time });
    if mirrored { if let Some(p) = world.get(id) { let mut e = p.edit.clone(); e.mirrored = true; world.set_edit(id, e); } }
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    // Cubes as cooked (`ConnectivityCubeData` of EMP_Wall_Solid / EMP_Floor_Floor / EMP_Stair_StairW / EMP_Roof_RoofC, 1.8 paks).
    // The data test in tests/data.rs checks these equal what PieceCatalog::load reads.
    pub const WALL: ConnectivityCube = [0x000F_C63F, 0x0008_4210, 0, 0x0000_8421, 0x01F0_0000, 0x01F0_0000];
    pub const FLOOR: ConnectivityCube = [0x000F_8000, 0x000F_8000, 0x000F_8000, 0x000F_8000, 0, 0x01F8_D63F];
    pub const STAIR: ConnectivityCube = [0x000F_8000, 0x0008_0001, 0x0000_001F, 0x0000_8010, 0x0000_001F, 0x01F0_0000];

    #[test]
    fn poi_pieces_anchor_connected_pieces_and_own_their_slot() {
        let (pl, pr) = grid::snap_transform(PieceKind::Floor, Vec3::new(0., 0., 400.), Rot::yaw(0.), 0);
        let poi = PoiPiece::new(7, PieceKind::Floor, pl, pr.yaw, Vec3::ZERO, vec![]);
        let env = PlacementEnv { poi_pieces: std::sync::Arc::new(vec![poi.clone()]), ..Default::default() };
        let (nl, nr) = grid::snap_transform(PieceKind::Floor, Vec3::new(512., 0., 400.), Rot::yaw(0.), 0);
        assert!(anchored(&env, &voxel(PieceKind::Floor, FLOOR, nl, nr.yaw, false)));
        let (fl, fr) = grid::snap_transform(PieceKind::Floor, Vec3::new(2048., 0., 400.), Rot::yaw(0.), 0);
        assert!(!anchored(&env, &voxel(PieceKind::Floor, FLOOR, fl, fr.yaw, false)));
        assert_eq!(grid::key_for(PieceKind::Floor, pl, pr.yaw).1, poi.slot);
    }

    #[test]
    fn preview_rotation_is_remembered_per_piece_and_walls_only_flip() {
        let mut p = BuildPreview::default();
        p.rotate(true);
        assert_eq!(p.rotation_iterations, 2);
        p.rotate(true);
        assert_eq!(p.rotation_iterations, 0);
        p.set_piece(PieceKind::Stairs);
        p.rotate(true);
        assert_eq!(p.rotation_iterations, 1);
        p.rotate(false); p.rotate(false);
        assert_eq!(p.rotation_iterations, 3);
        p.set_piece(PieceKind::Wall);
        assert_eq!(p.rotation_iterations, 0);
        p.set_piece(PieceKind::Stairs);
        assert_eq!(p.rotation_iterations, 3);
        p.set_piece(PieceKind::Wall);
        p.rotation_iterations = 1;
        p.prepare_place();
        assert_eq!(p.rotation_iterations, 0);
        p.rotation_iterations = 2;
        p.prepare_place();
        assert_eq!(p.rotation_iterations, 2);
        p.set_piece(PieceKind::Floor);
        p.rotation_iterations = 3;
        p.prepare_place();
        assert_eq!(p.rotation_iterations, 3);
    }

    #[test]
    fn rotate_four_times_is_identity_and_mirror_twice_is_identity() {
        let mut c = STAIR;
        for _ in 0..4 { c = rotate_cube(c); }
        assert_eq!(c, STAIR);
        assert_eq!(mirror_cube(mirror_cube(STAIR)), STAIR);
    }

    #[test]
    fn wall_on_a_floor_edge_connects_floor_beside_floor_connects_diagonal_does_not() {
        let floor = |cx: f32, cy: f32| { let (l, r) = grid::snap_transform(PieceKind::Floor, Vec3::new(cx, cy, 0.), Rot::yaw(0.), 0); voxel(PieceKind::Floor, FLOOR, l, r.yaw, false) };
        let a = floor(0., 0.);
        assert!(connected(&a, &floor(512., 0.)));
        assert!(!connected(&a, &floor(512., 512.)), "corner only");
        let (wl, wr) = grid::snap_transform(PieceKind::Wall, Vec3::new(10., 10., 100.), Rot::yaw(0.), 0);
        let w = voxel(PieceKind::Wall, WALL, wl, wr.yaw, false);
        assert!(connected(&a, &w));
        // the same wall one level up does not touch the floor
        let w2 = voxel(PieceKind::Wall, WALL, wl + Vec3::Z * 384., wr.yaw, false);
        assert!(!connected(&a, &w2));
        // a ramp in the next cell (low end toward the floor) touches the floor edge
        let (sl, sr) = grid::snap_transform(PieceKind::Stairs, Vec3::new(512., 0., 200.), Rot::yaw(0.), 0);
        assert!(connected(&a, &voxel(PieceKind::Stairs, STAIR, sl, sr.yaw, false)) || connected(&floor(1024., 0.), &voxel(PieceKind::Stairs, STAIR, sl, sr.yaw, false)));
    }
}

pub fn obstruction_enabled() -> bool { fn_live::var("FN_BUILD_OBSTRUCT").map_or(true, |v| v != "0") }
