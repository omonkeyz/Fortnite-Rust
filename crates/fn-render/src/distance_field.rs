//! DISTANCE FIELD HOOK: mesh distance fields of the real 1.8 props, the ray-traced distance-field shadow cascade of the sun and the shared object
//! data the distance-field AO (`distance_field_ao.rs`) traces. Port of UE4.16 `DistanceFieldShadowing.cpp/.usf`,
//! `DistanceFieldObjectManagement.cpp` (object data), `DistanceFieldAtlas.cpp` (atlas) and `DirectionalLightComponent.cpp` (the ray-traced cascade).
//!
//! # Data
//! The cooked 1.8 static meshes CARRY their `FDistanceFieldVolumeData` (StaticMesh.cpp `FStaticMeshRenderData::Serialize`, "Inline the distance field
//! derived data for cooked builds"): all 85 prop meshes of the catalog have one for LOD 0, zlib-compressed (`r.DistanceFieldBuild.Compress=1` at cook
//! time) PF_G8 volumes (`r.DistanceFieldBuild.EightBit`, exe default 1), at most 64 voxels per axis (`MaxPerMeshResolution` 128 / 2). None of the meshes
//! serialises `bGenerateMeshDistanceField`, so the project cooked with `r.GenerateMeshDistanceFields=1` (StaticMesh.cpp:1306); the same project
//! setting is what makes `GetDistanceFieldShadowDistance` non-zero at run time. The volumes are decoded by `fn_mesh::DistanceFieldVolumeData` and handed
//! to the renderer as [`MeshDistanceField`]; nothing is regenerated.
//!
//! # Coordinates
//! Everything distance-field related works in "DF space": the renderer world (metres, Y up, right handed) mapped by `(x, y, z) -> (x, z, y) * 100`,
//! i.e. centimetres, Z up, LEFT handed: exactly the axis convention of the UE map the props were placed from (props.rs maps UE `(x, y, z)` to the game
//! as `(x, z, y) * 0.01`). The UE formulas (FPlane winding, FRotator, FindBestAxisVectors, the cone directions) are therefore used literally.
//! SUN-AXIS FIX (2026-10-05): `lighting::ue_to_world` (sun, sky) now uses the same swap, so `dir_to_df(to_sun)` is exactly the 1.8 light's UE direction
//! (before, the sun came in 90 deg of yaw off: UE `u` landed here as `(u.y, -u.x, u.z)`).
//!
//! # What is 1:1 and what is adapted (also in the crate README)
//! * 1:1: cascade split / sphere / fade regions, the snapped `WorldToShadow`, the shadow convex-hull object culling, the light-tile size
//!   (`ShadowBounds.W / r.DFShadowWorldTileSize + 1`, max 256), the scatter of the 4x4 low-poly stencil sphere into light tiles with the OBB test of
//!   `ShadowObjectCullPS`, the per-object data (`WorldToVolume`, `LocalPositionExtent - InvBlockSize`, UV scale/add, two-sided sign, MAD),
//!   `ShadowRayTraceThroughCulledObjects` step for step, the near fade of the cascade, its `BO_Min` combine with the CSM and the cascade transitions.
//! * Adapted: UE culls and scatters on the GPU; here the same tests run on the CPU (identical lists; the tile order of overflowing tiles differs,
//!   UE's is atomic-order dependent).
//! * DF SHADOW SCREEN PASS (2026-10-06): the trace runs in screen space at half resolution (`r.DFFullResolution 0`, `GAODownsampleFactor` 2) over the full
//!   depth pre-pass into the PF_G16R16F `RayTracedShadows` target (distance_field_shadow.rs), and the lit shaders apply UE's bilateral upsample per pixel
//!   (`lit_common.wgsl` `df_shadow_upsample`). It used to run per shaded fragment in the lit shaders (the `r.DFFullResolution 1` look, about 1.7 ms of the
//!   scene pass at 2544 x 1353).
use crate::camera::CameraParams;
use crate::lighting;
use crate::static_mesh::StaticMeshInstance;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec2, Vec3, Vec4};

/// Values the 1.8 exe / assets / UE4.16 source decide. Every one is cited.
pub mod ue {
    /// `UDirectionalLightComponent::DistanceFieldShadowDistance` (cm): not serialized in TODM_A / TODM_A_Child_Athena, so the 1.8 exe class constructor
    /// value applies (reports/exe-defaults.json, store RVA 0x20a4805 = 30000.0; UE4.16 ctor DirectionalLightComponent.cpp:700 is the same).
    pub const DISTANCE_FIELD_SHADOW_DISTANCE_CM: f32 = 30000.;
    /// `bUseRayTracedDistanceFieldShadows`: SERIALIZED true on `DayNightDirectionalLightComponent` of TODM_A (fn-inspect props).
    pub const USE_RAY_TRACED_DF_SHADOWS: bool = true;
    /// `UDirectionalLightComponent::TraceDistance` (cm): exe ctor RVA 0x20a480f = 10000.0, proxy clamps to 1000..1e6 (DirectionalLightComponent.cpp:118).
    pub const TRACE_DISTANCE_CM: f32 = 10000.;
    /// `ULightComponent::RayStartOffsetDepthScale`: exe ctor RVA 0x20a4e1b = 0.003 (not serialized).
    pub const RAY_START_OFFSET_DEPTH_SCALE: f32 = 0.003;
    /// `r.DFTwoSidedMeshDistanceBias` (cm): 1.8 exe registration default 4.0 (exe-defaults.json cvars; UE4.16 DistanceFieldShadowing.cpp:63).
    pub const TWO_SIDED_MESH_DISTANCE_BIAS_CM: f32 = 4.;
    /// `r.DFShadowWorldTileSize` (cm): exe default 200 (DistanceFieldShadowing.cpp:55).
    pub const SHADOW_WORLD_TILE_SIZE_CM: f32 = 200.;
    /// `r.AOMaxObjectsPerCullTile`: exe default 256 (UE4.16 source default 512, DistanceFieldAmbientOcclusion.cpp:94). Also `ShadowMaxObjectsPerTile`.
    pub const MAX_OBJECTS_PER_CULL_TILE: u32 = 256;
    /// `r.AOMaxObjectBoundingRadius` (cm): exe default 50000 (DistanceFieldObjectManagement.cpp:22).
    pub const AO_MAX_OBJECT_BOUNDING_RADIUS_CM: f32 = 50000.;
    /// `r.DistanceFields.AtlasSizeXY` / `AtlasSizeZ`: exe defaults 512 / 1024 (DistanceFieldAtlas.cpp:76).
    pub const ATLAS_SIZE_XY: u32 = 512;
    pub const ATLAS_SIZE_Z: u32 = 1024;
    /// `GLowPolyStencilSphereVertexBuffer`: `TStencilSphereVertexBuffer<4, 4>` (ShadowRendering.cpp:137): 4 sides, 4 rings.
    pub const LOW_POLY_SPHERE_SIDES: usize = 4;
    pub const LOW_POLY_SPHERE_RINGS: usize = 4;
    /// `UStaticMesh::DistanceFieldSelfShadowBias`: not serialized by any prop mesh; the UE4.16 `UStaticMesh` ctor leaves it 0 (StaticMesh.cpp:1383).
    pub const DEFAULT_SELF_SHADOW_BIAS: f32 = 0.;
    /// `HALF_WORLD_MAX` (UE4.16 EngineDefines.h: 1048576 / 2): `MinLightW` of a directional whole-scene shadow.
    pub const HALF_WORLD_MAX: f32 = 524288.;
    /// `DepthRangeClamp` of a directional whole-scene projection (ShadowSetup.cpp:665).
    pub const DEPTH_RANGE_CLAMP_CM: f32 = 5000.;
}

/// Light-source angle in radians as the shading uses it: `Clamp(LightSourceAngle, 0.001, 5) * PI / 180` (DistanceFieldShadowing.cpp:471).
pub fn light_source_angle_rad() -> f32 { lighting::LIGHT_SOURCE_ANGLE_DEG.clamp(0.001, 5.) * std::f32::consts::PI / 180. }

/// Renderer world point (metres) -> DF space (cm, Z up, left handed). See the module docs.
pub fn to_df(p: Vec3) -> Vec3 { Vec3::new(p.x, p.z, p.y) * 100. }
/// Renderer world direction -> DF space direction.
pub fn dir_to_df(v: Vec3) -> Vec3 { Vec3::new(v.x, v.z, v.y) }

/// The cooked distance field of one static mesh LOD 0 (`FDistanceFieldVolumeData`), already decompressed, PF_G8.
#[derive(Clone, Debug)]
pub struct MeshDistanceField {
    /// `Size` (voxels per axis, X fastest in `texels`).
    pub size: [u32; 3],
    /// `X*Y*Z` UNORM8 texels: `(d - DistanceMinMax.x) / (DistanceMinMax.y - DistanceMinMax.x)` of the volume-space distance `d`.
    pub texels: Vec<u8>,
    /// `LocalBoundingBox` (cm, UE mesh-local space).
    pub box_min: [f32; 3],
    pub box_max: [f32; 3],
    /// `DistanceMinMax` (volume-space distances).
    pub distance_min_max: [f32; 2],
    pub built_as_if_two_sided: bool,
    pub mesh_was_plane: bool,
    /// `UStaticMesh::DistanceFieldSelfShadowBias` (cm).
    pub self_shadow_bias: f32,
    /// `bAffectDistanceFieldLighting` (and `CastShadow`) of the placed component; false = the mesh is not part of the distance-field scene.
    pub affects_distance_field_lighting: bool,
}
impl MeshDistanceField {
    /// `IsValidDistanceFieldVolume`: `Size.GetMax() > 0` (DistanceFieldAtlas.cpp:491) and a matching texel count.
    pub fn is_valid(&self) -> bool {
        let n = self.size.iter().map(|&s| s as usize).product::<usize>();
        self.size.iter().copied().max().unwrap_or(0) > 0 && self.texels.len() == n
    }
    /// FX SHADOW SELF-SHADOW BIAS: the same cooked volume (everything the atlas block and the object transform come from), whatever the self-shadow bias / flags.
    pub fn same_volume(&self, o: &MeshDistanceField) -> bool {
        self.size == o.size && self.box_min == o.box_min && self.box_max == o.box_max && self.distance_min_max == o.distance_min_max
            && self.built_as_if_two_sided == o.built_as_if_two_sided && self.mesh_was_plane == o.mesh_was_plane && self.texels == o.texels
    }
    pub fn extent(&self) -> Vec3 { (Vec3::from(self.box_max) - Vec3::from(self.box_min)) * 0.5 }
    pub fn center(&self) -> Vec3 { (Vec3::from(self.box_max) + Vec3::from(self.box_min)) * 0.5 }
}

// ------------------------------------------------------------------------------------------------------------------------------------------
// atlas
// ------------------------------------------------------------------------------------------------------------------------------------------
/// Allocation of every mesh's block in the 3D atlas (`FDistanceFieldVolumeTextureAtlas`). UE packs with `FTextureLayout3d`, largest volume first; the
/// placement does not change any sampled value (samples are clamped to block texel centres by `LocalPositionExtent - InvBlockSize`), so a simple
/// shelf packer in the same 512 x 512 x <=1024 volume is used.
#[derive(Clone, Debug, Default)]
pub struct AtlasLayout { pub size: [u32; 3], pub blocks: Vec<Option<[u32; 3]>> }
pub fn pack_atlas(sizes: &[Option<[u32; 3]>]) -> AtlasLayout {
    let mut order: Vec<usize> = (0..sizes.len()).filter(|&i| sizes[i].is_some()).collect();
    // FCompareVolumeAllocation: largest volume first
    order.sort_by_key(|&i| { let s = sizes[i].unwrap(); std::cmp::Reverse((s[0] as u64 * s[1] as u64 * s[2] as u64, s[2], s[1], i)) });
    let xy = ue::ATLAS_SIZE_XY;
    let mut blocks = vec![None; sizes.len()];
    let (mut x, mut y, mut z) = (0u32, 0u32, 0u32);
    let (mut row_h, mut slab_d, mut used_z) = (0u32, 0u32, 0u32);
    for i in order {
        let s = sizes[i].unwrap();
        if s[0] > xy || s[1] > xy { continue; }
        if x + s[0] > xy { x = 0; y += row_h; row_h = 0; }
        if y + s[1] > xy { x = 0; y = 0; z += slab_d; slab_d = 0; row_h = 0; }
        if z + s[2] > ue::ATLAS_SIZE_Z { continue; } // "Failed to allocate ... in distance field atlas"
        blocks[i] = Some([x, y, z]);
        x += s[0];
        row_h = row_h.max(s[1]);
        slab_d = slab_d.max(s[2]);
        used_z = used_z.max(z + s[2]);
    }
    AtlasLayout { size: [xy, xy, used_z.max(1)], blocks }
}

// ------------------------------------------------------------------------------------------------------------------------------------------
// object data (DistanceFieldObjectManagement.cpp ProcessPrimitiveUpdate)
// ------------------------------------------------------------------------------------------------------------------------------------------
/// vec4s per object in the GPU buffer (a subset of UE's OBJECT_DATA_STRIDE 18 + the bounds: what the traces read).
pub const OBJECT_VEC4S: usize = 9;

/// One placed distance-field object in DF space.
#[derive(Clone, Copy, Debug)]
pub struct DfObject {
    /// `ObjectBoundingSphere`: origin of `VolumeToWorld` and the length of its scale vector.
    pub center: Vec3,
    pub radius: f32,
    /// `LocalToWorld` (UE mesh-local cm -> DF space).
    pub local_to_world: Mat4,
    pub world_to_volume: Mat4,
    /// `LocalPositionExtent - InvBlockSize` (the half-texel clamp, DistanceFieldObjectManagement.cpp:878).
    pub local_position_extent: Vec3,
    pub uv_scale: Vec3,
    /// `VolumeScale` (max axis scale of `UniformScaleVolumeToWorld`), negative when built as if two sided.
    pub signed_volume_scale: f32,
    pub uv_add: Vec3,
    pub self_shadow_bias: f32,
    /// `DistanceFieldMAD` = (Max - Min, Min).
    pub mad: Vec2,
    /// `LocalVolumeBounds` (cm, mesh local).
    pub local_min: Vec3,
    pub local_max: Vec3,
    /// Index of the static mesh (renderer handle).
    pub mesh: usize,
}
impl DfObject {
    pub fn gpu(&self) -> [[f32; 4]; OBJECT_VEC4S] {
        let c = self.world_to_volume.to_cols_array_2d();
        [
            [self.center.x, self.center.y, self.center.z, self.radius],
            c[0], c[1], c[2], c[3],
            [self.local_position_extent.x, self.local_position_extent.y, self.local_position_extent.z, 0.],
            [self.uv_scale.x, self.uv_scale.y, self.uv_scale.z, self.signed_volume_scale],
            [self.uv_add.x, self.uv_add.y, self.uv_add.z, self.self_shadow_bias],
            [self.mad.x, self.mad.y, 0., 0.],
        ]
    }
}

/// `LocalToWorld` of a renderer static instance in DF space. Renderer mesh-local positions are UE mesh-local `(x, z, y) * 0.01` (fn-mesh glTF export);
/// the instance is `R_y(yaw) * (p * scale) + pos` (static_common.wgsl). Composed with the DF-space map this is a rotation about Z:
/// `X = (c Lx + s Ly) s_ + 100 pos.x`, `Y = (-s Lx + c Ly) s_ + 100 pos.z`, `Z = Lz s_ + 100 pos.y`.
/// MIRRORFIX: a mirrored instance (`mirror_x`, renderer-local x = UE-local X negated) negates the `Lx` column: the DF object's LocalToWorld keeps the
/// negative determinant like UE's (the volume lookup goes through its inverse, the distance itself is reflection invariant).
pub fn instance_local_to_world(inst: &StaticMeshInstance) -> Mat4 {
    let (s, c) = inst.yaw.sin_cos();
    let k = inst.scale;
    let mx = if inst.mirror_x { -1. } else { 1. };
    Mat4::from_cols(
        Vec4::new(c * k * mx, -s * k * mx, 0., 0.),
        Vec4::new(s * k, c * k, 0., 0.),
        Vec4::new(0., 0., k, 0.),
        to_df(inst.pos).extend(1.),
    )
}

/// The object data of one instance (`ProcessPrimitiveUpdate`): `None` for degenerate / too large primitives.
pub fn build_object(mesh: usize, df: &MeshDistanceField, block_min: [u32; 3], atlas_size: [u32; 3], mut local_to_world: Mat4) -> Option<DfObject> {
    let max_scale = local_to_world.x_axis.truncate().length().max(local_to_world.y_axis.truncate().length()).max(local_to_world.z_axis.truncate().length());
    if max_scale <= 0. { return None; } // "Skip degenerate primitives"
    if df.mesh_was_plane {
        // bMeshWasPlane: Z scale replaced by the smaller of the X / Y scales (keeping the sign), DistanceFieldObjectManagement.cpp:837
        let s = Vec3::new(local_to_world.x_axis.truncate().length(), local_to_world.y_axis.truncate().length(), local_to_world.z_axis.truncate().length());
        let mid = s.x.min(s.y);
        let adjust = mid / s.z.max(1e-12);
        local_to_world.z_axis = (local_to_world.z_axis.truncate() * adjust).extend(0.);
    }
    let extent = df.extent();
    let center = df.center();
    let block = Vec3::new(df.size[0] as f32, df.size[1] as f32, df.size[2] as f32);
    let inv_tex = Vec3::new(1. / atlas_size[0] as f32, 1. / atlas_size[1] as f32, 1. / atlas_size[2] as f32);
    // VolumeToWorld = Scale(Extent) * Translation(Center) * LocalToWorld (row vectors) = L2W * T(c) * S(e) (column vectors)
    let volume_to_world = local_to_world * Mat4::from_translation(center) * Mat4::from_scale(extent);
    let bounds_center = volume_to_world.w_axis.truncate();
    let scale_vec = Vec3::new(volume_to_world.x_axis.truncate().length(), volume_to_world.y_axis.truncate().length(), volume_to_world.z_axis.truncate().length());
    let radius = scale_vec.length();
    if radius >= ue::AO_MAX_OBJECT_BOUNDING_RADIUS_CM { return None; } // single-instance primitives must have BoundingRadius < r.AOMaxObjectBoundingRadius
    let max_extent = extent.max_element();
    let uniform_v2w = local_to_world * Mat4::from_translation(center) * Mat4::from_scale(Vec3::splat(max_extent));
    let volume_scale = uniform_v2w.x_axis.truncate().length().max(uniform_v2w.y_axis.truncate().length()).max(uniform_v2w.z_axis.truncate().length());
    let local_position_extent = extent / max_extent;
    let uv_scale_full = block * inv_tex;
    Some(DfObject {
        center: bounds_center,
        radius,
        local_to_world,
        world_to_volume: uniform_v2w.inverse(),
        local_position_extent: local_position_extent - Vec3::ONE / block,
        uv_scale: block * inv_tex * 0.5 / local_position_extent,
        signed_volume_scale: if df.built_as_if_two_sided { -volume_scale } else { volume_scale },
        uv_add: Vec3::new(block_min[0] as f32, block_min[1] as f32, block_min[2] as f32) * inv_tex + 0.5 * uv_scale_full,
        self_shadow_bias: df.self_shadow_bias,
        mad: Vec2::new(df.distance_min_max[1] - df.distance_min_max[0], df.distance_min_max[0]),
        local_min: Vec3::from(df.box_min),
        local_max: Vec3::from(df.box_max),
        mesh,
    })
}

// ------------------------------------------------------------------------------------------------------------------------------------------
// the ray-traced cascade (DirectionalLightComponent.cpp + ShadowSetup.cpp SetupWholeSceneProjection)
// ------------------------------------------------------------------------------------------------------------------------------------------
/// UE `FPlane(A, B, C)`: normal `((B - A) ^ (C - A)).GetSafeNormal()`, `W = A | Normal`; `PlaneDot(P) = (P | Normal) - W`.
pub fn ue_plane(a: Vec3, b: Vec3, c: Vec3) -> Vec4 {
    let n = (b - a).cross(c - a).normalize_or_zero();
    n.extend(a.dot(n))
}

/// `ComputeShadowCullingVolume` (DirectionalLightComponent.cpp:399): the convex hull of the cascade's camera sub-frustum extruded towards the light.
/// `light_direction` points TOWARDS the light (`-GetDirection()`).
pub fn shadow_culling_volume(v: &[Vec3; 8], light_direction: Vec3) -> Vec<Vec4> {
    const ADJACENT_PLANE_PAIRS: [[usize; 2]; 12] = [[0, 2], [0, 4], [0, 1], [0, 3], [2, 3], [4, 2], [1, 4], [3, 1], [2, 5], [4, 5], [1, 5], [3, 5]];
    const LINE_VERTEX_INDICES: [[usize; 2]; 12] = [[0, 1], [1, 3], [3, 2], [2, 0], [0, 4], [1, 5], [3, 7], [2, 6], [4, 5], [5, 7], [7, 6], [6, 4]];
    let sub = [
        ue_plane(v[3], v[2], v[0]), // Near
        ue_plane(v[7], v[6], v[2]), // Left
        ue_plane(v[0], v[4], v[5]), // Right
        ue_plane(v[2], v[6], v[4]), // Top
        ue_plane(v[5], v[7], v[3]), // Bottom
        ue_plane(v[4], v[6], v[7]), // Far
    ];
    let mut planes = vec![];
    for p in &sub { if p.truncate().dot(light_direction) < 0. { planes.push(*p); } }
    for i in 0..12 {
        let na = sub[ADJACENT_PLANE_PAIRS[i][0]].truncate();
        let nb = sub[ADJACENT_PLANE_PAIRS[i][1]].truncate();
        let (da, db) = (na.dot(light_direction), nb.dot(light_direction));
        if da * db < 0. {
            let a = v[LINE_VERTEX_INDICES[i][0]];
            let b = v[LINE_VERTEX_INDICES[i][1]];
            let c = a + light_direction * (a - b).length();
            planes.push(if da >= 0. { ue_plane(a, b, c) } else { ue_plane(b, a, c) });
        }
    }
    planes
}

/// `ComputeAccumulatedScale` (DirectionalLightComponent.cpp:481).
fn accumulated_scale(exponent: f32, idx: usize, count: usize) -> f32 {
    if idx == 0 { return 0.; }
    let (mut cur, mut total, mut ret) = (1f32, 0f32, 0f32);
    for i in 0..count { if i < idx { ret += cur; } total += cur; cur *= exponent; }
    ret / total
}

/// The camera as `GetShadowSplitBoundsDepthRange` sees it, in DF space.
#[derive(Clone, Copy, Debug)]
pub struct DfView {
    pub origin: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
    /// tan of the horizontal half FOV (`1 / Proj[0][0]`) and the aspect `Proj[1][1] / Proj[0][0]`.
    pub tan_half_h: f32,
    pub aspect: f32,
    /// `View.NearClippingDistance` (cm).
    pub near: f32,
}
impl DfView {
    pub fn from_camera(cam: &CameraParams) -> DfView {
        let (tx, ty) = cam.tan_half();
        let right = cam.forward.cross(cam.up).normalize();
        DfView { origin: to_df(cam.eye), right: dir_to_df(right), up: dir_to_df(cam.up.normalize()), forward: dir_to_df(cam.forward.normalize()), tan_half_h: tx, aspect: tx / ty, near: cam.near * 100. }
    }
}

/// Everything the shading and the light-tile culling need about the ray-traced cascade this frame.
#[derive(Clone, Debug)]
pub struct RtCascade {
    pub split_near: f32,
    pub split_far: f32,
    /// `SplitNearFadeRegion` (cm): `(BeforeSplitFar - BeforeSplitNear) * CascadeTransitionFraction`.
    pub split_near_fade_region: f32,
    /// `GetShadowSplitBounds` sphere (DF space, cm) = `ShadowBounds`.
    pub bounds_center: Vec3,
    pub bounds_radius: f32,
    /// `CascadeSettings.ShadowBoundsAccurate.Planes` (normal, W).
    pub hull: Vec<Vec4>,
    /// `FTranslationMatrix(PreShadowTranslation) * SubjectAndReceiverMatrix` (DF space -> shadow clip, column vectors).
    pub world_to_shadow: Mat4,
    /// `LightTileDimensions` (square).
    pub tiles: u32,
    /// Direction towards the light (DF space).
    pub to_light: Vec3,
}

/// `ShouldCreateRayTracedCascade` (DirectionalLightComponent.cpp:340): with `GetNumShadowMappedCascades(10)` CSM cascades (> 0 whenever the scaled CSM
/// distance is), the DF distance exceeds `GetCSMMaxDistance` (scaled by `r.Shadow.DistanceScale`); without CSM cascades any DF distance > 0 does.
/// `r.DistanceFieldShadowing` is NOT part of it (the cascade is created, its projection is skipped by `SupportsDistanceFieldShadows`).
pub fn should_create_ray_traced_cascade(csm: &lighting::CsmSetup) -> bool {
    let num_cascades = if csm.distance_m > 0. { lighting::CASCADE_COUNT.min(10) } else { 0 };
    let df = ue::DISTANCE_FIELD_SHADOW_DISTANCE_CM;
    ue::USE_RAY_TRACED_DF_SHADOWS && ((num_cascades > 0 && df > csm.distance_m * 100.) || (num_cascades == 0 && df > 0.))
}

/// The ray-traced cascade for this view (`GetShadowSplitBounds(View, INDEX_NONE)` + `GetViewDependentWholeSceneProjectedShadowInitializer` +
/// `SetupWholeSceneProjection`). `to_light` points towards the sun (DF space); `shadow_map_size` is `MaxCSMShadowResolution` (the RT cascade is set up
/// with the same `ResolutionX` as the CSM, ShadowSetup.cpp:2650, which only matters for the 4-texel snap of its origin).
/// `csm` = the scalability inputs (`NumNearCascades` = `View.MaxShadowCascades`-limited count, scaled CSM distance, scaled transition fraction).
pub fn rt_cascade(view: &DfView, to_light: Vec3, shadow_map_size: u32, csm: &lighting::CsmSetup) -> RtCascade {
    let num_near = csm.num_cascades;
    let csm_max = csm.distance_m * 100.;
    let split = |i: usize| -> f32 {
        // GetSplitDistance: index > NumNearCascades is the DF cascade end
        if i > num_near { ue::DISTANCE_FIELD_SHADOW_DISTANCE_CM } else { view.near + accumulated_scale(lighting::CASCADE_EXPONENT, i, num_near) * (csm_max - view.near) }
    };
    let split_near = split(num_near);
    let split_far = split(num_near + 1);
    // the DF cascade is the last one: no SplitFar extension; SplitNearFadeRegion from the previous (shadow-mapped) split
    // (only when ShadowSplitIndex >= 1, DirectionalLightComponent.cpp:651)
    let split_near_fade_region = if num_near >= 1 { (split(num_near) - split(num_near - 1)) * csm.transition_fraction } else { 0. };

    // GetShadowSplitBoundsDepthRange
    let tan_x = view.tan_half_h;
    let tan_y = tan_x / view.aspect;
    let (start_h, end_h) = (split_near * tan_x, split_far * tan_x);
    let (start_v, end_v) = (start_h / view.aspect, end_h / view.aspect);
    let o = view.origin;
    let (f, r, u) = (view.forward, view.right, view.up);
    let verts = [
        o + f * split_near + r * start_h + u * start_v,
        o + f * split_near + r * start_h - u * start_v,
        o + f * split_near - r * start_h + u * start_v,
        o + f * split_near - r * start_h - u * start_v,
        o + f * split_far + r * end_h + u * end_v,
        o + f * split_far + r * end_h - u * end_v,
        o + f * split_far - r * end_h + u * end_v,
        o + f * split_far - r * end_h - u * end_v,
    ];
    let frustum_length = split_far - split_near;
    let (far_x, far_y) = (tan_x * split_far, tan_y * split_far);
    let (near_x, near_y) = (tan_x * split_near, tan_y * split_near);
    let optimal = ((near_x * near_x + near_y * near_y) - (far_x * far_x + far_y * far_y)) / (2. * frustum_length) + frustum_length * 0.5;
    let centre_z = (split_far - optimal).clamp(split_near, split_far);
    let center = o + f * centre_z;
    let radius = verts.iter().map(|v| v.distance_squared(center)).fold(0f32, f32::max).sqrt().max(1.);
    let hull = shadow_culling_volume(&verts, to_light);

    // SetupWholeSceneProjection: WorldToLight = FInverseRotationMatrix(GetDirection().Rotation()), Scales (1, 1/W, 1/W), FaceDirection (1,0,0) ->
    // XAxis (0,0,1), YAxis (0,1,0) (FindBestAxisVectors), WorldToFace = WorldToLightScaled * FBasisVectorMatrix(-XAxis, YAxis, Face)
    let d = -to_light;
    let yaw = d.y.atan2(d.x);
    let pitch = d.z.atan2((d.x * d.x + d.y * d.y).sqrt());
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let ax = Vec3::new(cp * cy, cp * sy, sp);
    let ay = Vec3::new(-sy, cy, 0.);
    let az = Vec3::new(-sp * cy, -sp * sy, cp);
    let w = radius;
    // face(p) = (-p.az / W, p.ay / W, p.ax) (rotation + scale only, no translation)
    let world_to_face = Mat4::from_cols(
        Vec4::new(-az.x / w, ay.x / w, ax.x, 0.),
        Vec4::new(-az.y / w, ay.y / w, ax.y, 0.),
        Vec4::new(-az.z / w, ay.z / w, ax.z, 0.),
        Vec4::W,
    );
    // Max/MinSubjectZ around the (translated) subject origin, clamped by DepthRangeClamp
    let max_z = radius.max(ue::DEPTH_RANGE_CLAMP_CM);
    let min_z = (radius - 2. * radius).max(-ue::HALF_WORLD_MAX).min(-ue::DEPTH_RANGE_CLAMP_CM);
    // texel snap of PreShadowTranslation (MaxDownsampleFactor 4)
    let res = shadow_map_size.saturating_sub(2 * lighting::SHADOW_BORDER).max(1) as f32;
    let t = world_to_face.transform_point3(center);
    let snap = Vec3::new(t.x % (2. * 4. / res), t.y % (2. * 4. / res), 0.);
    let snapped = world_to_face.inverse().transform_point3(t - snap);
    // FShadowProjectionMatrix(MinZ, MaxZ, (0,0,0,1)): z' = (z - MinZ) / (MaxZ - MinZ)
    let proj = Mat4::from_cols(Vec4::X, Vec4::Y, Vec4::new(0., 0., 1. / (max_z - min_z), 0.), Vec4::new(0., 0., -min_z / (max_z - min_z), 1.));
    let world_to_shadow = proj * world_to_face * Mat4::from_translation(-snapped);
    let tiles = (radius / ue::SHADOW_WORLD_TILE_SIZE_CM + 1.).min(256.) as u32;
    RtCascade { split_near, split_far, split_near_fade_region, bounds_center: center, bounds_radius: radius, hull, world_to_shadow, tiles: tiles.max(1), to_light }
}

/// `ShadowConvexHullIntersectSphere` (DistanceFieldShadowing.usf:80) with `ShadowBoundingSphere = 0` (directional light: world-space planes).
pub fn hull_intersects_sphere(hull: &[Vec4], c: Vec3, r: f32) -> bool {
    hull.iter().all(|p| p.truncate().dot(c) - p.w <= r)
}

/// Light-tile lists of the culled objects: `heads[t] = (start, count)` into `indices` (object indices into the scene object list), counts capped at
/// `ShadowMaxObjectsPerTile`.
#[derive(Clone, Debug, Default)]
pub struct LightTiles { pub dim: u32, pub heads: Vec<[u32; 2]>, pub indices: Vec<u32>, pub culled: usize }

/// 2D convex hull (monotone chain), counter-clockwise.
fn convex_hull(mut p: Vec<Vec2>) -> Vec<Vec2> { let mut h = vec![]; convex_hull_into(&mut p, &mut h); h }
/// PERF (2026-10-08): [`convex_hull`] into reused buffers (`p` is sorted / deduplicated in place, the hull replaces `h`): the same operations.
fn convex_hull_into(p: &mut Vec<Vec2>, h: &mut Vec<Vec2>) {
    h.clear();
    p.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap().then(a.y.partial_cmp(&b.y).unwrap()));
    p.dedup_by(|a, b| (*a - *b).length_squared() < 1e-12);
    if p.len() < 3 { h.extend_from_slice(p); return; }
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    for &q in p.iter() {
        while h.len() >= 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0. { h.pop(); }
        h.push(q);
    }
    let lower = h.len() + 1;
    for &q in p.iter().rev().skip(1) {
        while h.len() >= lower && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0. { h.pop(); }
        h.push(q);
    }
    h.pop();
}
fn inside_convex(h: &[Vec2], q: Vec2) -> bool {
    if h.len() < 3 { return false; }
    (0..h.len()).all(|i| { let a = h[i]; let b = h[(i + 1) % h.len()]; (b - a).perp_dot(q - a) >= 0. })
}

/// `CullObjectsForShadowCS` + `ShadowObjectCullVS/PS` on the CPU.
/// PERF (2026-10-05): UE4.16 runs this on the GPU (DistanceFieldShadowing.cpp CullDistanceFieldObjectsForLight: a compute pass + the sphere rasterisation
/// appending to the tile lists); here the objects are split over scoped worker threads and merged in object order, so every tile list is exactly the
/// serial result (ascending object index, the first `MAX_OBJECTS_PER_CULL_TILE` kept).
pub fn cull_objects_to_light_tiles(rt: &RtCascade, objects: &[DfObject]) -> LightTiles {
    let n = rt.tiles;
    // PERF (2026-10-06, freezes): serial on the render thread. The 2026-10-05 split spawned up to 4 OS threads every frame (`std::thread::scope`); under
    // load (other apps, the game's own workers) the spawn / join waits made this 4-24 ms for the same input, and CommandEncoder::finish / Queue::submit
    // right after it slowed down too (first_use: finish p95 25-33 ms with the threads, 5.7 ms without). With the row-interval scan below the serial cull
    // is ~0.5 ms a frame (first_use mean 0.46, p95 0.65). `FN_DF_TILE_THREADS=N` (debug-only) still splits it over N scoped workers (same lists).
    static FORCED: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let threads = FORCED.get_or_init(|| fn_live::var("FN_DF_TILE_THREADS").ok().and_then(|v| v.parse().ok())).map_or(1, |t: usize| t.clamp(1, 16).min(objects.len().max(1)));
    let chunk = objects.len().div_ceil(threads).max(1);
    let parts: Vec<(Vec<(u32, u32)>, usize)> = if threads == 1 {
        vec![cull_object_range(rt, objects, 0)]
    } else {
        std::thread::scope(|s| {
            let hs: Vec<_> = objects.chunks(chunk).enumerate().map(|(ci, ch)| s.spawn(move || cull_object_range(rt, ch, ci * chunk))).collect();
            hs.into_iter().map(|h| h.join().expect("light tile cull worker")).collect()
        })
    };
    // merge in object order: per tile the first MAX_OBJECTS_PER_CULL_TILE entries (ascending object index)
    let cap = ue::MAX_OBJECTS_PER_CULL_TILE;
    let mut counts = vec![0u32; (n * n) as usize];
    let mut culled = 0usize;
    for (pairs, c) in &parts { culled += c; for &(t, _) in pairs { let k = &mut counts[t as usize]; if *k < cap { *k += 1; } } }
    let mut heads = Vec::with_capacity(counts.len());
    let mut total = 0u32;
    for &k in &counts { heads.push([total, k]); total += k; }
    let mut indices = vec![0u32; total as usize];
    let mut fill = vec![0u32; counts.len()];
    for (pairs, _) in &parts {
        for &(t, oi) in pairs {
            let f = &mut fill[t as usize];
            if *f < counts[t as usize] { indices[(heads[t as usize][0] + *f) as usize] = oi; *f += 1; }
        }
    }
    LightTiles { dim: n, heads, indices, culled }
}

/// PERF (debug-only stats, `FN_DF_TILE_STATS`): tile tests since the last report.
pub(crate) static TILE_TESTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The objects `objects` (global indices from `first`) against the light tiles: (tile, object) pairs in object order, and the hull-culled count.
fn cull_object_range(rt: &RtCascade, objects: &[DfObject], first: usize) -> (Vec<(u32, u32)>, usize) {
    let mut tests: u64 = 0; // PERF (debug-only stats): tile tests of the row scans
    let n = rt.tiles;
    let mut out: Vec<(u32, u32)> = vec![];
    let w2s = rt.world_to_shadow;
    // the 4x4 stencil sphere, unit radius (ShadowRendering.h TStencilSphereVertexBuffer::InitRHI)
    let mut unit = vec![];
    let (sides, rings) = (ue::LOW_POLY_SPHERE_SIDES, ue::LOW_POLY_SPHERE_RINGS);
    for s in 0..=sides {
        let yaw = (360. * (s as f32 / sides as f32)).to_radians();
        let (sy, cy) = yaw.sin_cos();
        for v in 0..=rings {
            let a = v as f32 * std::f32::consts::PI / rings as f32;
            let arc = Vec3::new(0., a.sin(), a.cos());
            // FRotationMatrix(FRotator(0, yaw, 0)).TransformPosition: x*X + y*Y + z*Z with X = (cy, sy, 0), Y = (-sy, cy, 0), Z = (0, 0, 1)
            unit.push(Vec3::new(cy, sy, 0.) * arc.x + Vec3::new(-sy, cy, 0.) * arc.y + Vec3::Z * arc.z);
        }
    }
    // FShadowObjectCullVS::SetParameters: ConservativeRadiusScale = 1 / cos(PI / NumRings), MinRadius = 2 * ShadowRadius / min(NumGroups)
    let conservative = 1. / (std::f32::consts::PI / rings as f32).cos();
    let min_radius = 2. * rt.bounds_radius / n as f32;
    let nf = n as f32;
    let mut culled = 0usize;
    for (k, o) in objects.iter().enumerate() {
        let oi = first + k;
        if !hull_intersects_sphere(&rt.hull, o.center, o.radius) { continue; }
        culled += 1;
        // CulledObjectBoxBounds: the 8 local volume corners in shadow space
        let (lo, hi) = (o.local_min, o.local_max);
        let corners = [
            Vec3::new(lo.x, lo.y, lo.z), Vec3::new(hi.x, lo.y, lo.z), Vec3::new(lo.x, hi.y, lo.z), Vec3::new(hi.x, hi.y, lo.z),
            Vec3::new(lo.x, lo.y, hi.z), Vec3::new(hi.x, lo.y, hi.z), Vec3::new(lo.x, hi.y, hi.z), Vec3::new(hi.x, hi.y, hi.z),
        ];
        let vs: Vec<Vec3> = corners.iter().map(|c| w2s.transform_point3(o.local_to_world.transform_point3(*c))).collect();
        let (mut bmin, mut bmax) = (Vec3::splat(2e6), Vec3::splat(-2e6));
        for v in &vs { bmin = bmin.min(*v); bmax = bmax.max(*v); }
        let axis = |a: Vec3| a / a.dot(a).max(1e-4);
        let (axx, axy, axz) = (axis((vs[1] - vs[0]) * 0.5), axis((vs[2] - vs[0]) * 0.5), axis((vs[4] - vs[0]) * 0.5));
        let obj_center = 0.5 * (bmin + bmax);
        // rasterised footprint of the conservative sphere in light-tile pixels (D3D viewport: x = (cx * .5 + .5) * n, y = (1 - (cy * .5 + .5)) * n)
        let eff = (o.radius + min_radius) * conservative;
        let pts: Vec<Vec2> = unit.iter().map(|u| { let c = w2s.transform_point3(*u * eff + o.center); Vec2::new((c.x * 0.5 + 0.5) * nf, (1. - (c.y * 0.5 + 0.5)) * nf) }).collect();
        let hull2 = convex_hull(pts);
        if hull2.len() < 3 { continue; }
        let (mut pmin, mut pmax) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for p in &hull2 { pmin = pmin.min(*p); pmax = pmax.max(*p); }
        let x0 = (pmin.x - 0.5).ceil().max(0.) as i32;
        let x1 = ((pmax.x - 0.5).floor() as i32).min(n as i32 - 1);
        let y0 = (pmin.y - 0.5).ceil().max(0.) as i32;
        let y1 = ((pmax.y - 0.5).floor() as i32).min(n as i32 - 1);
        // PERF: the same float operations as the per-tile form (kept as `cull_object_range_reference` in the tests), hoisted out of the tile loops:
        // the hull edges once per object, the OBB corner products once per tile column / row (glam's dot = (x*x' + y*y') + z*z', no contraction).
        let edges: Vec<(Vec2, Vec2)> = (0..hull2.len()).map(|i| (hull2[i], hull2[(i + 1) % hull2.len()] - hull2[i])).collect();
        let inside = |q: Vec2| edges.iter().all(|(a, e)| e.perp_dot(q - *a) >= 0.);
        let zl = [-1000. - obj_center.z, 1. - obj_center.z];
        let zt = [[zl[0] * axx.z, zl[0] * axy.z, zl[0] * axz.z], [zl[1] * axx.z, zl[1] * axy.z, zl[1] * axz.z]];
        for ty in y0..=y1 {
            let tpy = nf - ty as f32;
            let (ylo, yhi) = ((tpy - 1.) / nf * 2. - 1., (tpy + 1.) / nf * 2. - 1.);
            if !(bmax.y > ylo && bmin.y < yhi) { continue; }
            let yl = [ylo - obj_center.y, yhi - obj_center.y];
            let yt = [[yl[0] * axx.y, yl[0] * axy.y, yl[0] * axz.y], [yl[1] * axx.y, yl[1] * axy.y, yl[1] * axz.y]];
            let pass = |tx: i32| -> bool {
                if !inside(Vec2::new(tx as f32 + 0.5, ty as f32 + 0.5)) { return false; }
                // ShadowObjectCullPS OBB_INTERSECTION
                let tpx = tx as f32;
                let (xlo, xhi) = ((tpx - 1.) / nf * 2. - 1., (tpx + 1.) / nf * 2. - 1.);
                if !(bmax.x > xlo && bmin.x < xhi && bmin.z < 1.) { return false; }
                let xl = [xlo - obj_center.x, xhi - obj_center.x];
                let xt = [[xl[0] * axx.x, xl[0] * axy.x, xl[0] * axz.x], [xl[1] * axx.x, xl[1] * axy.x, xl[1] * axz.x]];
                let (mut minp, mut maxp) = (Vec3::splat(500000.), Vec3::splat(-500000.));
                for i in 0..8 {
                    let (a, b, c) = (&xt[i & 1], &yt[(i >> 1) & 1], &zt[(i >> 2) & 1]);
                    let pr = Vec3::new(a[0] + b[0] + c[0], a[1] + b[1] + c[1], a[2] + b[2] + c[2]);
                    minp = minp.min(pr);
                    maxp = maxp.max(pr);
                }
                minp.x < 1. && minp.y < 1. && minp.z < 1. && maxp.x > -1. && maxp.y > -1. && maxp.z > -1.
            };
            // PERF (2026-10-06, freezes): the tiles of a row that pass form one interval, in float arithmetic too: every hull test is
            // `C - e.y * ((tx + .5) - a.x) >= 0` (monotone in tx), the tile's x bounds grow with tx, and every OBB corner projection on an axis moves
            // with tx in the same direction (min / max over the corners stay monotone): each test holds on a half-line of tx. So the first passing
            // tile from each end bounds the row and every tile between passes; only the tiles outside are tested (the per-tile reference form in the
            // tests checks the identity).
            let mut l = x0;
            while l <= x1 && !pass(l) { l += 1; tests += 1; }
            tests += 1;
            if l > x1 { continue; }
            let mut r = x1;
            while r > l && !pass(r) { r -= 1; tests += 1; }
            tests += 1;
            for tx in l..=r { out.push((ty as u32 * n + tx as u32, oi as u32)); }
        }
    }
    TILE_TESTS.fetch_add(tests, std::sync::atomic::Ordering::Relaxed);
    (out, culled)
}

// ------------------------------------------------------------------------------------------------------------------------------------------
// PERF (2026-10-08): the light-tile cull with view-independent data prepared once per object list (`CullPrep`), reused buffers (`CullScratch`)
// and the GPU words written directly. EXACT: every per-object / per-tile float operation of `cull_object_range` is kept in the same order (the
// tests compare the words bit for bit), only work whose result cannot change is skipped:
//  * the 8 volume corners in DF space (`local_to_world.transform_point3`) are computed when the object list changes, not every frame;
//  * a coarse xy grid of the object centres: a cell whose centre box, grown by its largest radius, lies wholly outside one plane of the cascade's
//    culling hull holds no object that passes `hull_intersects_sphere` (every object of the cell fails that plane), so its objects are not tested;
//    the other objects are tested exactly, in ascending order;
//  * no per-object heap allocations.
// `FN_DF_TILE_FAST=0` = the old path (`cull_objects_to_light_tiles`), `FN_DF_TILE_VERIFY=1` = both, words compared every frame (debug).
// ------------------------------------------------------------------------------------------------------------------------------------------
/// Cell size (cm, DF space xy) of [`CullPrep`]'s grid. CHOSEN: 50 m.
pub const CULL_GRID_CM: f32 = 5000.;
/// View-independent data of the light-tile cull for one object list.
#[derive(Clone, Debug, Default)]
pub struct CullPrep {
    /// Per object: the 8 local volume corners in DF space (the order `cull_object_range` builds them in).
    corners: Vec<[Vec3; 8]>,
    /// Per object: its grid cell.
    cell_of: Vec<u32>,
    /// Per cell: (centre box min, centre box max, largest radius); empty cells have min > max.
    cells: Vec<(Vec3, Vec3, f32)>,
    /// This frame: the cell may hold an object inside the hull.
    open: Vec<bool>,
}
impl CullPrep {
    pub fn build(objects: &[DfObject]) -> CullPrep {
        let corners = objects.iter().map(|o| {
            let (lo, hi) = (o.local_min, o.local_max);
            [
                Vec3::new(lo.x, lo.y, lo.z), Vec3::new(hi.x, lo.y, lo.z), Vec3::new(lo.x, hi.y, lo.z), Vec3::new(hi.x, hi.y, lo.z),
                Vec3::new(lo.x, lo.y, hi.z), Vec3::new(hi.x, lo.y, hi.z), Vec3::new(lo.x, hi.y, hi.z), Vec3::new(hi.x, hi.y, hi.z),
            ].map(|c| o.local_to_world.transform_point3(c))
        }).collect();
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for o in objects { if o.center.is_finite() { lo = lo.min(o.center.truncate()); hi = hi.max(o.center.truncate()); } }
        let (nx, ny) = if lo.x <= hi.x { ((((hi.x - lo.x) / CULL_GRID_CM) as u32 + 1).min(512), (((hi.y - lo.y) / CULL_GRID_CM) as u32 + 1).min(512)) } else { (1, 1) };
        // one extra cell (the last) for anything non-finite: never closed
        let mut cells = vec![(Vec3::splat(f32::MAX), Vec3::splat(f32::MIN), 0f32); (nx * ny) as usize + 1];
        let spill = (nx * ny) as usize;
        let cell_of = objects.iter().map(|o| {
            if !(o.center.is_finite() && o.radius.is_finite()) { return spill as u32; }
            let cx = (((o.center.x - lo.x) / CULL_GRID_CM) as u32).min(nx - 1);
            let cy = (((o.center.y - lo.y) / CULL_GRID_CM) as u32).min(ny - 1);
            let c = (cy * nx + cx) as usize;
            let e = &mut cells[c];
            e.0 = e.0.min(o.center); e.1 = e.1.max(o.center); e.2 = e.2.max(o.radius);
            c as u32
        }).collect();
        let open = vec![true; cells.len()];
        CullPrep { corners, cell_of, cells, open }
    }
    pub fn len(&self) -> usize { self.corners.len() }
    /// Marks the cells that may hold an object passing `hull_intersects_sphere(hull, ..)` (conservative: a margin covers float rounding).
    fn open_cells(&mut self, hull: &[Vec4]) {
        let spill = self.cells.len() - 1;
        for (k, (bmin, bmax, r)) in self.cells.iter().enumerate() {
            self.open[k] = k == spill || bmin.x > bmax.x || !hull.iter().any(|p| {
                let n = p.truncate();
                // the smallest n . c over the centre box
                let d = (if n.x > 0. { n.x * bmin.x } else { n.x * bmax.x }) + (if n.y > 0. { n.y * bmin.y } else { n.y * bmax.y }) + (if n.z > 0. { n.z * bmin.z } else { n.z * bmax.z });
                let slack = 1e-4 * (d.abs() + p.w.abs() + r) + 1.;
                d - p.w > r + slack
            });
        }
    }
}
/// Reused buffers of [`cull_light_tiles_prepared`].
#[derive(Clone, Debug, Default)]
pub struct CullScratch { pairs: Vec<(u32, u32)>, pts: Vec<Vec2>, hull: Vec<Vec2>, edges: Vec<(Vec2, Vec2)>, counts: Vec<u32>, fill: Vec<u32> }
/// What [`cull_light_tiles_prepared`] reports besides the words (the [`LightTiles`] stats).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LightTileStats { pub dim: u32, pub entries: usize, pub culled: usize }

/// The light-tile lists as the GPU words the shadow pass reads: `[base + start, count]` per tile, then the object indices (`base` = 2 * tiles),
/// exactly what `prepare` builds from [`cull_objects_to_light_tiles`]. `prep` must have been built from `objects`.
pub fn cull_light_tiles_prepared(rt: &RtCascade, objects: &[DfObject], prep: &mut CullPrep, sc: &mut CullScratch, words: &mut Vec<u32>) -> LightTileStats {
    debug_assert_eq!(prep.len(), objects.len());
    let mut tests: u64 = 0;
    let n = rt.tiles;
    let w2s = rt.world_to_shadow;
    prep.open_cells(&rt.hull);
    sc.pairs.clear();
    // the 4x4 stencil sphere, unit radius (ShadowRendering.h TStencilSphereVertexBuffer::InitRHI), as in `cull_object_range`
    let mut unit = [Vec3::ZERO; (ue::LOW_POLY_SPHERE_SIDES + 1) * (ue::LOW_POLY_SPHERE_RINGS + 1)];
    let (sides, rings) = (ue::LOW_POLY_SPHERE_SIDES, ue::LOW_POLY_SPHERE_RINGS);
    let mut k = 0;
    for s in 0..=sides {
        let yaw = (360. * (s as f32 / sides as f32)).to_radians();
        let (sy, cy) = yaw.sin_cos();
        for v in 0..=rings {
            let a = v as f32 * std::f32::consts::PI / rings as f32;
            let arc = Vec3::new(0., a.sin(), a.cos());
            unit[k] = Vec3::new(cy, sy, 0.) * arc.x + Vec3::new(-sy, cy, 0.) * arc.y + Vec3::Z * arc.z;
            k += 1;
        }
    }
    let conservative = 1. / (std::f32::consts::PI / rings as f32).cos();
    let min_radius = 2. * rt.bounds_radius / n as f32;
    let nf = n as f32;
    let mut culled = 0usize;
    for (oi, o) in objects.iter().enumerate() {
        if !prep.open[prep.cell_of[oi] as usize] { continue; }
        if !hull_intersects_sphere(&rt.hull, o.center, o.radius) { continue; }
        culled += 1;
        let wc = &prep.corners[oi];
        let vs: [Vec3; 8] = std::array::from_fn(|i| w2s.transform_point3(wc[i]));
        let (mut bmin, mut bmax) = (Vec3::splat(2e6), Vec3::splat(-2e6));
        for v in &vs { bmin = bmin.min(*v); bmax = bmax.max(*v); }
        let axis = |a: Vec3| a / a.dot(a).max(1e-4);
        let (axx, axy, axz) = (axis((vs[1] - vs[0]) * 0.5), axis((vs[2] - vs[0]) * 0.5), axis((vs[4] - vs[0]) * 0.5));
        let obj_center = 0.5 * (bmin + bmax);
        let eff = (o.radius + min_radius) * conservative;
        sc.pts.clear();
        sc.pts.extend(unit.iter().map(|u| { let c = w2s.transform_point3(*u * eff + o.center); Vec2::new((c.x * 0.5 + 0.5) * nf, (1. - (c.y * 0.5 + 0.5)) * nf) }));
        convex_hull_into(&mut sc.pts, &mut sc.hull);
        let hull2 = &sc.hull;
        if hull2.len() < 3 { continue; }
        let (mut pmin, mut pmax) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for p in hull2.iter() { pmin = pmin.min(*p); pmax = pmax.max(*p); }
        let x0 = (pmin.x - 0.5).ceil().max(0.) as i32;
        let x1 = ((pmax.x - 0.5).floor() as i32).min(n as i32 - 1);
        let y0 = (pmin.y - 0.5).ceil().max(0.) as i32;
        let y1 = ((pmax.y - 0.5).floor() as i32).min(n as i32 - 1);
        sc.edges.clear();
        sc.edges.extend((0..hull2.len()).map(|i| (hull2[i], hull2[(i + 1) % hull2.len()] - hull2[i])));
        let edges = &sc.edges;
        let inside = |q: Vec2| edges.iter().all(|(a, e)| e.perp_dot(q - *a) >= 0.);
        let zl = [-1000. - obj_center.z, 1. - obj_center.z];
        let zt = [[zl[0] * axx.z, zl[0] * axy.z, zl[0] * axz.z], [zl[1] * axx.z, zl[1] * axy.z, zl[1] * axz.z]];
        for ty in y0..=y1 {
            let tpy = nf - ty as f32;
            let (ylo, yhi) = ((tpy - 1.) / nf * 2. - 1., (tpy + 1.) / nf * 2. - 1.);
            if !(bmax.y > ylo && bmin.y < yhi) { continue; }
            let yl = [ylo - obj_center.y, yhi - obj_center.y];
            let yt = [[yl[0] * axx.y, yl[0] * axy.y, yl[0] * axz.y], [yl[1] * axx.y, yl[1] * axy.y, yl[1] * axz.y]];
            let pass = |tx: i32| -> bool {
                if !inside(Vec2::new(tx as f32 + 0.5, ty as f32 + 0.5)) { return false; }
                let tpx = tx as f32;
                let (xlo, xhi) = ((tpx - 1.) / nf * 2. - 1., (tpx + 1.) / nf * 2. - 1.);
                if !(bmax.x > xlo && bmin.x < xhi && bmin.z < 1.) { return false; }
                let xl = [xlo - obj_center.x, xhi - obj_center.x];
                let xt = [[xl[0] * axx.x, xl[0] * axy.x, xl[0] * axz.x], [xl[1] * axx.x, xl[1] * axy.x, xl[1] * axz.x]];
                let (mut minp, mut maxp) = (Vec3::splat(500000.), Vec3::splat(-500000.));
                for i in 0..8 {
                    let (a, b, c) = (&xt[i & 1], &yt[(i >> 1) & 1], &zt[(i >> 2) & 1]);
                    let pr = Vec3::new(a[0] + b[0] + c[0], a[1] + b[1] + c[1], a[2] + b[2] + c[2]);
                    minp = minp.min(pr);
                    maxp = maxp.max(pr);
                }
                minp.x < 1. && minp.y < 1. && minp.z < 1. && maxp.x > -1. && maxp.y > -1. && maxp.z > -1.
            };
            let mut l = x0;
            while l <= x1 && !pass(l) { l += 1; tests += 1; }
            tests += 1;
            if l > x1 { continue; }
            let mut r = x1;
            while r > l && !pass(r) { r -= 1; tests += 1; }
            tests += 1;
            for tx in l..=r { sc.pairs.push((ty as u32 * n + tx as u32, oi as u32)); }
        }
    }
    TILE_TESTS.fetch_add(tests, std::sync::atomic::Ordering::Relaxed);
    // per tile the first MAX_OBJECTS_PER_CULL_TILE entries in object order, straight into the words
    let cap = ue::MAX_OBJECTS_PER_CULL_TILE;
    let nt = (n * n) as usize;
    sc.counts.clear(); sc.counts.resize(nt, 0);
    for &(t, _) in &sc.pairs { let k = &mut sc.counts[t as usize]; if *k < cap { *k += 1; } }
    let base = (nt * 2) as u32;
    words.clear();
    words.reserve(nt * 2);
    let mut total = 0u32;
    for &k in &sc.counts { words.push(base + total); words.push(k); total += k; }
    words.resize(nt * 2 + total as usize, 0);
    sc.fill.clear(); sc.fill.resize(nt, 0);
    for &(t, oi) in &sc.pairs {
        let t = t as usize;
        let f = &mut sc.fill[t];
        if *f < sc.counts[t] { let at = (words[2 * t] + *f) as usize; words[at] = oi; *f += 1; }
    }
    LightTileStats { dim: n, entries: total as usize, culled }
}
/// The words `prepare` uploads for [`cull_objects_to_light_tiles`]'s lists (the old path; reference of [`cull_light_tiles_prepared`]).
pub fn light_tile_words(tiles: &LightTiles) -> Vec<u32> {
    let mut words: Vec<u32> = Vec::with_capacity(tiles.heads.len() * 2 + tiles.indices.len());
    let base = (tiles.heads.len() * 2) as u32;
    for h in &tiles.heads { words.push(base + h[0]); words.push(h[1]); }
    words.extend_from_slice(&tiles.indices);
    words
}
fn env_flag(name: &str, default: bool) -> bool { fn_live::var(name).map_or(default, |v| v != "0") }

/// GDF PARTIAL UPDATES (2026-10-06): the `PrimitiveModifiedBounds` of an object-list rebuild: the bounding spheres (centre, radius) of the objects of
/// `old` that are not in `new` (removed, or moved: the previous bounds) and of `new` that are not in `old` (added, or moved: the new bounds). Objects are
/// matched by mesh and exact `LocalToWorld` (a rebuild recomputes unchanged instances bit for bit), as a multiset.
pub fn modified_object_bounds(old: &[DfObject], new: &[DfObject]) -> Vec<Vec4> {
    use std::collections::HashMap;
    let key = |o: &DfObject| (o.mesh, o.local_to_world.to_cols_array().map(f32::to_bits));
    let p = old.iter().zip(new).take_while(|(a, b)| key(a) == key(b)).count();
    let (old, new) = (&old[p..], &new[p..]);
    let mut count: HashMap<(usize, [u32; 16]), i32, std::hash::BuildHasherDefault<FxHasher>> = HashMap::with_capacity_and_hasher(old.len(), Default::default());
    for o in old { *count.entry(key(o)).or_insert(0) += 1; }
    let mut out = vec![];
    for o in new {
        match count.get_mut(&key(o)) {
            Some(k) if *k > 0 => *k -= 1,
            _ => out.push(o.center.extend(o.radius)),
        }
    }
    // what is left of `old` was removed (or moved away)
    for o in old.iter().rev() {
        if let Some(k) = count.get_mut(&key(o)) { if *k > 0 { *k -= 1; out.push(o.center.extend(o.radius)); } }
    }
    out
}

#[derive(Default)]
pub(crate) struct FxHasher(u64);
pub(crate) type FxBuild = std::hash::BuildHasherDefault<FxHasher>;
impl std::hash::Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut c = bytes.chunks_exact(8);
        for w in &mut c { self.add(u64::from_le_bytes(w.try_into().unwrap())); }
        for &b in c.remainder() { self.add(b as u64); }
    }
    #[inline]
    fn write_u32(&mut self, i: u32) { self.add(i as u64); }
    #[inline]
    fn write_u64(&mut self, i: u64) { self.add(i); }
    #[inline]
    fn write_usize(&mut self, i: usize) { self.add(i as u64); }
    #[inline]
    fn finish(&self) -> u64 { self.0.rotate_left(26) }
}
impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) { self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95); }
}

/// `GetShadowTileCulledData` (DistanceFieldShadowingShared.usf:27): the light tile of a DF-space point.
pub fn light_tile_of(rt: &RtCascade, p: Vec3) -> Option<(u32, u32)> {
    let c = rt.world_to_shadow.transform_point3(p);
    let nx = c.x * 0.5 + 0.5;
    let ny = 1. - (c.y * 0.5 + 0.5);
    if !(0. ..1.).contains(&nx) || !(0. ..1.).contains(&ny) { return None; }
    Some(((nx * rt.tiles as f32) as u32, (ny * rt.tiles as f32) as u32))
}

// ------------------------------------------------------------------------------------------------------------------------------------------
// GPU
// ------------------------------------------------------------------------------------------------------------------------------------------
/// `struct DfU` of lit_common.wgsl (group 0 binding 5).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct DfUniform {
    /// x enabled, y MinDepth (cm), z MaxDepth (cm), w NearFadePlaneOffset (cm)
    pub sh0: [f32; 4],
    /// x InvNearFadePlaneLength (1/cm), y TanLightAngle, z TraceDistance (cm), w RayStartOffsetDepthScale
    pub sh1: [f32; 4],
    /// x TwoSidedMeshDistanceBias (cm), y/z light tiles x/y, w ShadowMaxObjectsPerTile
    pub sh2: [f32; 4],
    /// xyz direction towards the light (DF space), w whole-scene shadow fade far distance (m)
    pub sh3: [f32; 4],
    pub world_to_shadow: [[f32; 4]; 4],
    /// x AO valid, y OcclusionExponent, z MinOcclusion, w OcclusionCombineMode (0 = OCM_Minimum, 1 = OCM_Multiply)
    pub ao0: [f32; 4],
    /// xyz OcclusionTint (linear), w unused
    pub ao1: [f32; 4],
    /// ContrastAndNormalizeMulAdd
    pub ao2: [f32; 4],
    /// AO buffer (floor(W/2), floor(H/2), 1/x, 1/y)
    pub ao3: [f32; 4],
    /// renderer world -> clip of the frame the AO buffer was computed for
    pub prev_view_proj: [[f32; 4]; 4],
    pub prev_eye: [f32; 4],
    pub prev_fwd: [f32; 4],
    /// DF SHADOW SCREEN PASS: xy `RayTracedShadows` size (`GetBufferSizeForDFShadows`), z `GetDFShadowDownsampleFactor()`, w 1 = bilateral upsample
    pub sh4: [f32; 4],
}

/// Globals bindings of the distance-field block (lit_common.wgsl): 5 = [`DfUniform`], 10 = the DFAO bent normal, 16 = this frame's `RayTracedShadows`.
/// DF SHADOW SCREEN PASS (2026-10-06): the trace inputs (6 objects, 7 light tiles, 8 atlas, 9 its sampler) moved to the screen pass's own bind group
/// (distance_field_shadow.rs): the lit shaders no longer trace (per fragment stage: 2 storage buffers and 1 sampler fewer, the same texture count).
pub(crate) fn globals_layout_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    let f = wgpu::ShaderStages::FRAGMENT;
    let buf = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry { binding, visibility: f, ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None }, count: None };
    let tex2 = |binding: u32| wgpu::BindGroupLayoutEntry { binding, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None };
    vec![buf(5, wgpu::BufferBindingType::Uniform), tex2(10), tex2(16)]
}

struct MeshEntry { df: MeshDistanceField }
/// DF ATLAS: an atlas volume: a static mesh (renderer handle) or an FX mesh (index into `dyn_meshes`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AtlasKey { Static(usize), Dyn(usize) }

/// Per-frame numbers (`FrameStats`-style, for logs / tests).
#[derive(Clone, Debug, Default)]
pub struct DfStats { pub meshes: usize, pub objects: usize, pub shadow_culled: usize, pub light_tiles: u32, pub tile_entries: usize, pub atlas: [u32; 3] }

/// The distance-field scene of the renderer: atlas, object buffer, the ray-traced cascade and the DFAO passes.
pub(crate) struct DistanceFields {
    meshes: Vec<Option<MeshEntry>>,
    instances: Vec<StaticMeshInstance>,
    objects: Vec<DfObject>,
    objects_dirty: bool,
    /// DF ATLAS (2026-10-06, freezes): the volume allocation of UE4.16's atlas (distance_field_atlas.rs: `FTextureLayout3d` + `UpdateAllocations`) and its
    /// texture. A new volume is uploaded into its own block; the atlas is rebuilt (new texture, everything uploaded, every object's atlas coordinates and the
    /// global distance field redone) only when the layout outgrows the texture (`Generation` change).
    atlas: crate::distance_field_atlas::VolumeAtlas<AtlasKey>,
    atlas_tex: Option<wgpu::Texture>,
    old_atlas: Vec<wgpu::Texture>,
    /// PERF: whole-atlas rebuilds so far (each one is logged).
    atlas_rebuilds: u32,
    layout: AtlasLayout,
    pub(crate) uniform_buf: wgpu::Buffer,
    pub(crate) objects_buf: wgpu::Buffer,
    objects_cap: usize,
    tiles_buf: wgpu::Buffer,
    tiles_cap: usize,
    pub(crate) atlas_view: wgpu::TextureView,
    pub(crate) sampler: wgpu::Sampler,
    pub(crate) ao: crate::distance_field_ao::DfAo,
    /// The globals bind group must be rebuilt (a buffer / texture it references was re-created).
    bind_dirty: bool,
    pub stats: DfStats,
    pub(crate) rt: Option<RtCascade>,
    /// Bumped whenever the objects buffer or the atlas is re-created.
    pub(crate) resource_gen: u64,
    /// FX SHADOW: distance fields of the FX static meshes (player-built pieces), keyed by the `Arc` of [`crate::fx::FxDfInstance::mesh`] (atlas blocks after the
    /// statics' ones), and this frame's placed instances (index into `dyn_meshes`, LocalToWorld in DF space).
    dyn_meshes: Vec<std::sync::Arc<MeshDistanceField>>,
    /// FX SHADOW SELF-SHADOW BIAS (2026-10-05): per instance (index into `dyn_meshes`, LocalToWorld, the proxy's `DistanceFieldSelfShadowBias`), and the
    /// `Arc`s that only differ from a `dyn_meshes` entry in `self_shadow_bias` (same volume: one atlas block, see [`Self::set_dynamic`]).
    dyn_instances: Vec<(usize, Mat4, f32)>,
    dyn_aliases: Vec<(std::sync::Arc<MeshDistanceField>, usize)>,
    /// PERF (2026-10-07): the `dyn_meshes` index of every `Arc` seen by [`Self::set_dynamic`] (by address: each one is kept alive in `dyn_meshes` / `dyn_aliases`,
    /// so an address is never reused), instead of a linear search per object per frame (thousands of placed statics x hundreds of meshes).
    dyn_index: std::collections::HashMap<usize, usize, std::hash::BuildHasherDefault<FxHasher>>,
    /// DF SHADOW SCREEN PASS (2026-10-06): the half-resolution ray trace of the cascade (distance_field_shadow.rs) and the generation of the light-tile buffer
    /// its bind group holds (bumped whenever `tiles_buf` is re-created).
    shadow_pass: crate::distance_field_shadow::DfShadowPass,
    /// PERF (2026-10-06, freezes): this frame's CPU split of [`Self::prepare`] (atlas, objects, gdf = DfAo::prepare, tiles = light-tile culling + upload), ms;
    /// the renderer reports them as `prep df <name>`.
    pub(crate) prep_ms: Vec<(&'static str, f32)>,
    tiles_gen: u64,
    /// PERF (2026-10-08): bumped whenever `objects` is rebuilt; the light-tile cull's prepared data ([`CullPrep`], built for `cull_prep_gen`), reused
    /// buffers, and the key of the words last uploaded to `tiles_buf` (a frame with the same objects and the same cascade reuses them).
    objects_gen: u64,
    cull_prep: CullPrep,
    cull_prep_gen: u64,
    cull_scratch: CullScratch,
    tile_words: Vec<u32>,
    tiles_key: Option<(u64, Vec<u32>)>,
    tile_stats: LightTileStats,
    /// PERF (report-only): the last [`Self::set_dynamic`] in ms, reported as `prep df dynamic`.
    dyn_ms: f32,
}

impl DistanceFields {
    pub fn new(device: &wgpu::Device) -> DistanceFields {
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("df uniform"), size: std::mem::size_of::<DfUniform>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let objects_buf = Self::storage(device, "df objects", 64 * OBJECT_VEC4S * 16);
        let tiles_buf = Self::storage(device, "df light tiles", 1024);
        let atlas_view = Self::atlas_texture(device, [1, 1, 1]).create_view(&Default::default());
        // DistanceFieldSampler: TStaticSamplerState<SF_Bilinear, AM_Clamp, AM_Clamp, AM_Clamp> (trilinear on a 3D texture)
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("df atlas sampler"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let ao = crate::distance_field_ao::DfAo::new(device);
        DistanceFields {
            meshes: vec![], instances: vec![], objects: vec![], objects_dirty: false, atlas: crate::distance_field_atlas::VolumeAtlas::new([ue::ATLAS_SIZE_XY, ue::ATLAS_SIZE_XY, ue::ATLAS_SIZE_Z]).with_texture_padding([ue::ATLAS_SIZE_XY, ue::ATLAS_SIZE_XY, 384]), atlas_tex: None, old_atlas: vec![], atlas_rebuilds: 0, layout: AtlasLayout::default(),
            uniform_buf, objects_buf, objects_cap: 64, tiles_buf, tiles_cap: 256, atlas_view, sampler, ao, bind_dirty: true, stats: DfStats::default(), rt: None, resource_gen: 0,
            dyn_meshes: vec![], dyn_instances: vec![], dyn_aliases: vec![], dyn_index: Default::default(),
            shadow_pass: crate::distance_field_shadow::DfShadowPass::new(device), tiles_gen: 0, prep_ms: vec![],
            objects_gen: 1, cull_prep: CullPrep::default(), cull_prep_gen: 0, cull_scratch: CullScratch::default(), tile_words: vec![], tiles_key: None, tile_stats: LightTileStats::default(), dyn_ms: 0.,
        }
    }
    fn storage(device: &wgpu::Device, label: &str, bytes: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes.max(16) as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
    }
    fn atlas_texture(device: &wgpu::Device, size: [u32; 3]) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("df atlas (PF_G8)"), size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: size[2] }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D3, format: wgpu::TextureFormat::R8Unorm, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC, view_formats: &[],
        })
    }

    pub fn bind_entries(&self) -> Vec<wgpu::BindGroupEntry<'_>> {
        vec![
            wgpu::BindGroupEntry { binding: 5, resource: self.uniform_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 10, resource: wgpu::BindingResource::TextureView(self.ao.scene_view()) },
            wgpu::BindGroupEntry { binding: 16, resource: wgpu::BindingResource::TextureView(self.shadow_pass.view()) }, // DF SHADOW SCREEN PASS
        ]
    }
    pub fn take_bind_dirty(&mut self) -> bool { std::mem::take(&mut self.bind_dirty) }

    pub fn set_mesh(&mut self, mesh: usize, df: MeshDistanceField) {
        if self.meshes.len() <= mesh { self.meshes.resize_with(mesh + 1, || None); }
        // DF ATLAS: a replaced volume leaves the atlas (UE: RemoveAllocation), the new one is a pending allocation (AddAllocation)
        if self.meshes[mesh].is_some() { self.atlas.remove(AtlasKey::Static(mesh)); }
        self.meshes[mesh] = (df.is_valid() && df.affects_distance_field_lighting).then_some(MeshEntry { df });
        if let Some(e) = &self.meshes[mesh] { self.atlas.add(AtlasKey::Static(mesh), e.df.size); }
        self.objects_dirty = true;
    }
    pub fn mesh_count(&self) -> usize { self.meshes.iter().filter(|m| m.is_some()).count() }
    pub fn set_instances(&mut self, inst: &[StaticMeshInstance]) {
        // shake / sway only move vertices (WPO); the primitive transform (what the DF scene uses) is pos / yaw / scale
        let same = inst.len() == self.instances.len() && inst.iter().zip(&self.instances).all(|(a, b)| a.mesh == b.mesh && a.pos == b.pos && a.yaw == b.yaw && a.scale == b.scale && a.mirror_x == b.mirror_x);
        if !same { self.instances = inst.to_vec(); self.objects_dirty = true; }
    }
    /// FX SHADOW: this frame's FX static meshes in the distance-field scene (UE adds every `FStaticMeshSceneProxy` with a distance field and
    /// `bAffectDistanceFieldLighting` to the object buffer, DistanceFieldObjectManagement.cpp `UpdateGlobalDistanceFieldObjectBuffers`, whatever placed it).
    /// A new mesh extends the atlas; the object buffer is rebuilt only when the list changed.
    /// FX SHADOW SELF-SHADOW BIAS: the object's `SelfShadowBias` is the instance's own `mesh.self_shadow_bias` (the proxy's value: `bOverrideDistanceFieldSelfShadowBias
    /// ? Component->DistanceFieldSelfShadowBias : StaticMesh->DistanceFieldSelfShadowBias`, clamped >= 0, 1.8 `FStaticMeshSceneProxy` ctor @14248b430); an `Arc`
    /// that only carries another bias than an already placed volume shares that volume's atlas block (UE uploads the mesh's volume once).
    pub fn set_dynamic(&mut self, objs: &[crate::fx::FxDfInstance]) {
        // PERF (2026-10-08): the list is compared with last frame's while it is walked (a copy is made only from the first difference on), and a run of
        // instances of the same `Arc` reuses the index found for the first one (the lookup gives the same k). Same `dyn_instances`, same dirty decisions.
        let t_dyn = std::time::Instant::now();
        let (mut inst, mut same, mut j, mut last): (Vec<(usize, Mat4, f32)>, bool, usize, Option<(usize, usize)>) = (vec![], true, 0, None);
        let mut put = |e: (usize, Mat4, f32), prev: &[(usize, Mat4, f32)]| {
            if same { if prev.get(j) == Some(&e) { j += 1; return; } same = false; inst = Vec::with_capacity(objs.len()); inst.extend_from_slice(&prev[..j]); }
            inst.push(e);
        };
        for o in objs {
            if !(o.mesh.is_valid() && o.mesh.affects_distance_field_lighting) { continue; }
            let addr = std::sync::Arc::as_ptr(&o.mesh) as usize;
            if let Some((a, k)) = last { if a == addr { put((k, o.local_to_world, o.mesh.self_shadow_bias.max(0.)), &self.dyn_instances); continue; } } // PERF: same Arc as the previous instance
            if let Some(&k) = self.dyn_index.get(&addr) { last = Some((addr, k)); put((k, o.local_to_world, o.mesh.self_shadow_bias.max(0.)), &self.dyn_instances); continue; } // PERF: seen before
            let found = self.dyn_meshes.iter().position(|m| std::sync::Arc::ptr_eq(m, &o.mesh))
                .or_else(|| self.dyn_aliases.iter().find(|(a, _)| std::sync::Arc::ptr_eq(a, &o.mesh)).map(|(_, k)| *k));
            let k = match found {
                Some(k) => k,
                None => match self.dyn_meshes.iter().position(|m| m.same_volume(&o.mesh)) {
                    Some(k) => { self.dyn_aliases.push((o.mesh.clone(), k)); k }
                    None => { self.dyn_meshes.push(o.mesh.clone()); self.atlas.add(AtlasKey::Dyn(self.dyn_meshes.len() - 1), o.mesh.size); self.objects_dirty = true; self.dyn_meshes.len() - 1 } // DF ATLAS: AddAllocation
                },
            };
            // PERF: the Arc is now held by `dyn_meshes` or `dyn_aliases` (a ptr_eq hit is one of them already)
            if !self.dyn_meshes.iter().any(|m| std::sync::Arc::ptr_eq(m, &o.mesh)) && !self.dyn_aliases.iter().any(|(a, _)| std::sync::Arc::ptr_eq(a, &o.mesh)) { self.dyn_aliases.push((o.mesh.clone(), k)); }
            self.dyn_index.insert(addr, k);
            last = Some((addr, k));
            put((k, o.local_to_world, o.mesh.self_shadow_bias.max(0.)), &self.dyn_instances);
        }
        drop(put);
        if !same || j != self.dyn_instances.len() { if same { inst = self.dyn_instances[..j].to_vec(); } self.dyn_instances = inst; self.objects_dirty = true; }
        self.dyn_ms = t_dyn.elapsed().as_secs_f32() * 1000.;
        static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *VERIFY.get_or_init(|| fn_live::var("FN_DF_TILE_VERIFY").map_or(false, |v| v == "1")) { // debug: the list the old form built (every Arc is in dyn_index now)
            let old: Vec<(usize, Mat4, f32)> = objs.iter().filter(|o| o.mesh.is_valid() && o.mesh.affects_distance_field_lighting).map(|o| (self.dyn_index[&(std::sync::Arc::as_ptr(&o.mesh) as usize)], o.local_to_world, o.mesh.self_shadow_bias.max(0.))).collect();
            if old != self.dyn_instances { eprintln!("[df dynamic] VERIFY MISMATCH: {} vs {} instances", self.dyn_instances.len(), old.len()); }
        }
    }

    /// DF ATLAS: the cooked volume of an atlas key: (size, texels).
    fn volume(&self, k: AtlasKey) -> Option<([u32; 3], &[u8])> {
        match k {
            AtlasKey::Static(i) => self.meshes.get(i)?.as_ref().map(|m| (m.df.size, m.df.texels.as_slice())),
            AtlasKey::Dyn(k) => self.dyn_meshes.get(k).map(|m| (m.size, m.texels.as_slice())),
        }
    }

    fn rebuild(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        for t in self.old_atlas.drain(..) { t.destroy(); }
        let t_rebuild = std::time::Instant::now(); // PERF: prep df atlas / objects
        // DF ATLAS (2026-10-06, freezes): UpdateAllocations (DistanceFieldAtlas.cpp:246-436, distance_field_atlas.rs). A volume that fits the current texture
        // is uploaded into its block alone (RHIUpdateTexture3D per allocation); only a layout that outgrew the texture rebuilds it (Generation++), which
        // changes every object's atlas coordinates: all objects are rebuilt and the global distance field recomposited (UE queues an update of every
        // primitive on a generation change, DistanceFieldObjectManagement.cpp:976-990). Was: a repack, a 16 MB upload and a full GDF update per new mesh.
        match self.atlas.update() {
            crate::distance_field_atlas::AtlasUpdate::None => {}
            crate::distance_field_atlas::AtlasUpdate::Blocks(keys) => {
                let t_atlas = std::time::Instant::now();
                let mut bytes = 0usize;
                if let Some(tex) = self.atlas_tex.as_ref() {
                    for k in &keys {
                        let (Some(b), Some((s, texels))) = (self.atlas.block(*k), self.volume(*k)) else { continue };
                        queue.write_texture(
                            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d { x: b[0], y: b[1], z: b[2] }, aspect: wgpu::TextureAspect::All },
                            texels,
                            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(s[0]), rows_per_image: Some(s[1]) },
                            wgpu::Extent3d { width: s[0], height: s[1], depth_or_array_layers: s[2] },
                        );
                        bytes += texels.len();
                    }
                }
                self.objects_dirty = true;
                eprintln!("[df] atlas: {} volume(s) added in place ({:.0} KB uploaded, generation {}) in {:.2} ms", keys.len(), bytes as f32 / 1024., self.atlas.generation, t_atlas.elapsed().as_secs_f32() * 1000.);
            }
            crate::distance_field_atlas::AtlasUpdate::Full { regenerated } => {
                let t_atlas = std::time::Instant::now(); // PERF: logged below (a whole-atlas rebuild + upload is a frame hitch)
                let size = self.atlas.texture_size().unwrap_or([1, 1, 1]);
                let tex = Self::atlas_texture(device, size);
                let previous = self.atlas.take_previous();
                let moved_on_gpu = if let (true, Some(old)) = (regenerated && !previous.is_empty(), self.atlas_tex.as_ref()) {
                    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("df atlas move") });
                    let (mut moved, mut uploaded) = (0usize, 0usize);
                    for &(k, s, b) in self.atlas.placed() {
                        match previous.iter().find(|p| p.0 == k) {
                            Some(&(_, from)) => {
                                enc.copy_texture_to_texture(
                                    wgpu::TexelCopyTextureInfo { texture: old, mip_level: 0, origin: wgpu::Origin3d { x: from[0], y: from[1], z: from[2] }, aspect: wgpu::TextureAspect::All },
                                    wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d { x: b[0], y: b[1], z: b[2] }, aspect: wgpu::TextureAspect::All },
                                    wgpu::Extent3d { width: s[0], height: s[1], depth_or_array_layers: s[2] },
                                );
                                moved += 1;
                            }
                            None => {
                                let Some((vs, texels)) = self.volume(k) else { continue };
                                queue.write_texture(
                                    wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d { x: b[0], y: b[1], z: b[2] }, aspect: wgpu::TextureAspect::All },
                                    texels,
                                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(vs[0]), rows_per_image: Some(vs[1]) },
                                    wgpu::Extent3d { width: vs[0], height: vs[1], depth_or_array_layers: vs[2] },
                                );
                                uploaded += 1;
                            }
                        }
                    }
                    queue.submit([enc.finish()]);
                    self.atlas_view = tex.create_view(&Default::default());
                    if let Some(old) = self.atlas_tex.replace(tex.clone()) { self.old_atlas.push(old); }
                    self.bind_dirty = true;
                    self.resource_gen += 1;
                    self.ao.atlas_changed();
                    self.objects_dirty = true;
                    self.atlas_rebuilds += 1;
                    if !self.atlas.failed.is_empty() { eprintln!("[df] atlas: failed to allocate {:?} in the distance field atlas", std::mem::take(&mut self.atlas.failed)); }
                    eprintln!("[df] atlas regenerated #{} (generation {}): {moved} volumes moved on the GPU, {uploaded} uploaded, {}x{}x{} in {:.1} ms", self.atlas_rebuilds, self.atlas.generation, size[0], size[1], size[2], t_atlas.elapsed().as_secs_f32() * 1000.);
                    true
                } else { false };
                if !moved_on_gpu {
                // the coalesced full update (one RHIUpdateTexture3D of the whole atlas)
                let (w, h, d) = (size[0] as usize, size[1] as usize, size[2] as usize);
                let slab = ((8usize << 20) / (w * h).max(1)).clamp(1, d.max(1));
                let mut data = vec![0u8; w * h * slab];
                let mut z0 = 0usize;
                while z0 < d {
                    let z1 = (z0 + slab).min(d);
                    let part = &mut data[..w * h * (z1 - z0)];
                    part.fill(0);
                    for &(k, _, b) in self.atlas.placed() {
                        let Some((s, texels)) = self.volume(k) else { continue };
                        let (bz0, bz1) = (b[2] as usize, b[2] as usize + s[2] as usize);
                        if bz1 <= z0 || bz0 >= z1 { continue; }
                        for z in bz0.max(z0)..bz1.min(z1) {
                            let lz = z - bz0;
                            for y in 0..s[1] as usize {
                                let src = (lz * s[1] as usize + y) * s[0] as usize;
                                let dst = ((z - z0) * h + b[1] as usize + y) * w + b[0] as usize;
                                part[dst..dst + s[0] as usize].copy_from_slice(&texels[src..src + s[0] as usize]);
                            }
                        }
                    }
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: z0 as u32 }, aspect: wgpu::TextureAspect::All },
                        &*part,
                        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size[0]), rows_per_image: Some(size[1]) },
                        wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: (z1 - z0) as u32 },
                    );
                    z0 = z1;
                }
                self.atlas_view = tex.create_view(&Default::default());
                if let Some(old) = self.atlas_tex.replace(tex) { self.old_atlas.push(old); }
                self.bind_dirty = true;
                self.resource_gen += 1;
                self.ao.atlas_changed();
                self.objects_dirty = true;
                self.atlas_rebuilds += 1;
                if !self.atlas.failed.is_empty() { eprintln!("[df] atlas: failed to allocate {:?} in the distance field atlas", std::mem::take(&mut self.atlas.failed)); }
                eprintln!("[df] atlas rebuild #{} ({}, generation {}): {} static + {} dynamic volumes, {}x{}x{} ({:.1} MB) in {:.1} ms", self.atlas_rebuilds, if regenerated { "the layout outgrew the texture" } else { "first" }, self.atlas.generation, self.meshes.iter().filter(|m| m.is_some()).count(), self.dyn_meshes.len(), size[0], size[1], size[2], (w * h * d) as f32 / (1024. * 1024.), t_atlas.elapsed().as_secs_f32() * 1000.);
                }
            }
        }
        self.prep_ms.push(("atlas", t_rebuild.elapsed().as_secs_f32() * 1000.));
        let t_objects = std::time::Instant::now();
        if self.objects_dirty {
            // the blocks by object mesh index (statics, then `meshes.len() + k` for the FX meshes) in the current texture
            self.layout = AtlasLayout {
                size: self.atlas.texture_size().unwrap_or([1, 1, 1]),
                blocks: (0..self.meshes.len()).map(|i| self.atlas.block(AtlasKey::Static(i))).chain((0..self.dyn_meshes.len()).map(|k| self.atlas.block(AtlasKey::Dyn(k)))).collect(),
            };
            self.objects_dirty = false;
            self.objects_gen += 1; // PERF (2026-10-08): the light-tile cull's prepared data / cached words are for the previous list
            let old = std::mem::take(&mut self.objects); // GDF PARTIAL UPDATES: diffed against the new list below
            for inst in &self.instances {
                let Some(Some(m)) = self.meshes.get(inst.mesh.0) else { continue };
                let Some(Some(block)) = self.layout.blocks.get(inst.mesh.0) else { continue };
                if let Some(o) = build_object(inst.mesh.0, &m.df, *block, self.layout.size, instance_local_to_world(inst)) { self.objects.push(o); }
            }
            // FX SHADOW: the FX static meshes (player-built pieces) placed this frame
            for (k, l2w, bias) in &self.dyn_instances {
                let mi = self.meshes.len() + k;
                let (Some(m), Some(Some(block))) = (self.dyn_meshes.get(*k), self.layout.blocks.get(mi)) else { continue };
                if let Some(mut o) = build_object(mi, m, *block, self.layout.size, *l2w) { o.self_shadow_bias = *bias; self.objects.push(o); } // FX SHADOW SELF-SHADOW BIAS
            }
            if self.objects.len() > self.objects_cap {
                self.objects_cap = self.objects.len().next_power_of_two();
                self.objects_buf = Self::storage(device, "df objects", self.objects_cap * OBJECT_VEC4S * 16);
                self.resource_gen += 1;
                self.bind_dirty = true;
            }
            if !self.objects.is_empty() {
                let data: Vec<[[f32; 4]; OBJECT_VEC4S]> = self.objects.iter().map(|o| o.gpu()).collect();
                queue.write_buffer(&self.objects_buf, 0, bytemuck::cast_slice(&data));
            }
            // GDF PARTIAL UPDATES (2026-10-06): UE4.16 records the bounding sphere of every distance-field object added, removed or moved
            // (`PrimitiveModifiedBounds`, DistanceFieldObjectManagement.cpp:590 / :923 / :928); the clipmaps recomposite those regions on their next update
            self.ao.objects_changed(&modified_object_bounds(&old, &self.objects));
        }
        self.prep_ms.push(("objects", t_objects.elapsed().as_secs_f32() * 1000.));
    }

    /// Per frame, before the shadow / scene passes: rebuild the atlas / objects when needed, set up the ray-traced cascade, cull the objects into the
    /// light tiles and upload the shading uniform. `shadows_on` = the light's dynamic shadows are drawn this frame.
    /// `ao_enabled` = the AO show flag + DFAO scalability; `depth_view` / `depth_gen` = this frame's scene depth (read by the DFAO passes after the scene pass).
    /// `shadows_on` must already include `r.DistanceFieldShadowing` (`SupportsDistanceFieldShadows`); `csm` = this frame's CSM scalability inputs.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, cam: &CameraParams, to_sun: Vec3, shadow_map_size: u32, shadows_on: bool,
                   ao_enabled: bool, depth_view: &wgpu::TextureView, depth_gen: u64, plate_half_m: f32, csm: &lighting::CsmSetup) {
        self.prep_ms.clear();
        self.prep_ms.push(("dynamic", std::mem::take(&mut self.dyn_ms))); // PERF (2026-10-08): set_dynamic, run just before (inside the renderer's "prep distance fields" mark)
        self.rebuild(device, queue);
        let t_gdf = std::time::Instant::now(); // PERF: prep df gdf
        {
            let inp = crate::distance_field_ao::AoFrameInput {
                cam, objects: &self.objects, objects_buf: &self.objects_buf, atlas_view: &self.atlas_view, atlas_sampler: &self.sampler, depth_view,
                resource_gen: depth_gen.wrapping_mul(1 << 20).wrapping_add(self.resource_gen), plate_half_m,
            };
            if self.ao.prepare(device, queue, &inp, ao_enabled) { self.bind_dirty = true; }
            self.prep_ms.push(("gdf", t_gdf.elapsed().as_secs_f32() * 1000.));
        }
        let view = DfView::from_camera(cam);
        let to_light = dir_to_df(to_sun).normalize();
        let active = shadows_on && should_create_ray_traced_cascade(csm) && !self.objects.is_empty();
        let t_tiles = std::time::Instant::now(); // PERF: prep df tiles
        let mut u = DfUniform::zeroed();
        if active {
            let rt = rt_cascade(&view, to_light, shadow_map_size, csm);
            let t_cull = std::time::Instant::now();
            // PERF (2026-10-08): `FN_DF_TILE_FAST=0` = the old cull (also with `FN_DF_TILE_THREADS`); `FN_DF_TILE_CACHE=0` = never reuse last frame's words;
            // `FN_DF_TILE_VERIFY=1` = the fast words are compared with the old path every frame (debug)
            static FAST: std::sync::OnceLock<(bool, bool, bool)> = std::sync::OnceLock::new();
            let (fast, cache, verify) = *FAST.get_or_init(|| (env_flag("FN_DF_TILE_FAST", true) && fn_live::var_os("FN_DF_TILE_THREADS").is_none(), env_flag("FN_DF_TILE_CACHE", true), fn_live::var("FN_DF_TILE_VERIFY").map_or(false, |v| v == "1")));
            // the cascade as bits: the same objects and the same cascade give the same words (the cull reads nothing else)
            let key = (self.objects_gen, std::iter::once(rt.tiles).chain(std::iter::once(rt.bounds_radius.to_bits())).chain(rt.world_to_shadow.to_cols_array().iter().map(|f| f.to_bits()))
                .chain(rt.hull.iter().flat_map(|p| p.to_array()).map(f32::to_bits)).collect::<Vec<u32>>());
            let reuse = fast && cache && self.tiles_key.as_ref() == Some(&key);
            let upload = if reuse { false } else if fast {
                if self.cull_prep_gen != self.objects_gen { self.cull_prep = CullPrep::build(&self.objects); self.cull_prep_gen = self.objects_gen; }
                self.tile_stats = cull_light_tiles_prepared(&rt, &self.objects, &mut self.cull_prep, &mut self.cull_scratch, &mut self.tile_words);
                true
            } else {
                let tiles = cull_objects_to_light_tiles(&rt, &self.objects);
                self.tile_stats = LightTileStats { dim: tiles.dim, entries: tiles.indices.len(), culled: tiles.culled };
                self.tile_words = light_tile_words(&tiles);
                true
            };
            if verify && fast {
                let old = light_tile_words(&cull_objects_to_light_tiles(&rt, &self.objects));
                if old != self.tile_words { eprintln!("[df tiles] VERIFY MISMATCH: {} words (fast{}) vs {} (old)", self.tile_words.len(), if reuse { ", reused" } else { "" }, old.len()); }
            }
            let tiles = self.tile_stats;
            // PERF (debug-only): `FN_DF_TILE_STATS=1` describes a light-tile cull slower than 4 ms (objects, culled, grid, entries, the largest objects)
            if t_cull.elapsed().as_secs_f32() > 0.004 && fn_live::var_os("FN_DF_TILE_STATS").is_some() {
                let mut big: Vec<(f32, usize)> = self.objects.iter().map(|o| (o.radius, o.mesh)).collect();
                big.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                eprintln!("[df tiles] {:.1} ms: {} objects, {} culled, grid {}x{} (radius {:.0} cm), {} entries, {} tile tests; largest (radius cm, mesh) {:?}", t_cull.elapsed().as_secs_f32() * 1000., self.objects.len(), tiles.culled, tiles.dim, tiles.dim, rt.bounds_radius, tiles.entries, TILE_TESTS.swap(0, std::sync::atomic::Ordering::Relaxed), &big[..big.len().min(5)]);
            }
            if upload {
                let words = &self.tile_words;
                if words.len() > self.tiles_cap {
                    self.tiles_cap = words.len().next_power_of_two();
                    self.tiles_buf = Self::storage(device, "df light tiles", self.tiles_cap * 4);
                    self.tiles_gen += 1; // DF SHADOW SCREEN PASS: the pass's bind group holds it (no longer the globals)
                }
                queue.write_buffer(&self.tiles_buf, 0, bytemuck::cast_slice(words));
                self.tiles_key = Some(key);
            }
            let tan = light_source_angle_rad().tan();
            // MinDepth = SplitNear - SplitNearFadeRegion, MaxDepth = SplitFar; NearFadePlaneOffset / InvNearFadePlaneLength (DistanceFieldShadowing.cpp:491 / 614)
            let min_depth = rt.split_near - rt.split_near_fade_region;
            u.sh0 = [if fn_live::var_os("FN_DEBUG_NO_DFSHADOW").is_some() /* PERF: debug-only A/B */ { 0. } else { 1. }, min_depth, rt.split_far, min_depth];
            u.sh1 = [1. / rt.split_near_fade_region.max(0.00001), tan, ue::TRACE_DISTANCE_CM.clamp(1000., 1e6), ue::RAY_START_OFFSET_DEPTH_SCALE];
            u.sh2 = [ue::TWO_SIDED_MESH_DISTANCE_BIAS_CM, rt.tiles as f32, rt.tiles as f32, ue::MAX_OBJECTS_PER_CULL_TILE as f32];
            // GetDirectionalLightDistanceFadeParameters: FarDistance = the DF shadow distance when the ray-traced cascade exists
            u.sh3 = [to_light.x, to_light.y, to_light.z, ue::DISTANCE_FIELD_SHADOW_DISTANCE_CM * 0.01];
            u.world_to_shadow = rt.world_to_shadow.to_cols_array_2d();
            self.stats.shadow_culled = tiles.culled;
            self.stats.light_tiles = tiles.dim;
            self.stats.tile_entries = tiles.entries;
            self.rt = Some(rt);
            // DF SHADOW SCREEN PASS (2026-10-06): RayTraceShadows at GetDFShadowDownsampleFactor() resolution into RayTracedShadows (distance_field_shadow.rs),
            // read by the lit shaders' upsample (sh4); the pass reconstructs world positions with this frame's jittered view (`cam` = the renderer's view camera)
            let factor = crate::distance_field_shadow::downsample_factor();
            let size = crate::distance_field_shadow::buffer_size(cam.width, cam.height, factor);
            if self.shadow_pass.ensure_target(device, size) { self.bind_dirty = true; }
            u.sh4 = [size[0] as f32, size[1] as f32, factor as f32, if factor > 1 { 1. } else { 0. }];
            let (tx, ty) = cam.tan_half();
            let dfs = crate::distance_field_shadow::DfsUniform {
                view_inverse: cam.view_matrix().inverse().to_cols_array_2d(),
                eye: [cam.eye.x, cam.eye.y, cam.eye.z, 0.],
                proj: [tx, ty, cam.near, cam.far],
                size: [cam.width as f32, cam.height as f32, 1. / cam.width.max(1) as f32, 1. / cam.height.max(1) as f32],
                misc: [cam.jitter[0], cam.jitter[1], factor as f32, 0.],
            };
            queue.write_buffer(&self.shadow_pass.uniform, 0, bytemuck::bytes_of(&dfs));
            self.shadow_pass.ensure_bind_group(device, (self.resource_gen, self.tiles_gen, depth_gen), &self.uniform_buf, &self.objects_buf, &self.tiles_buf, &self.atlas_view, &self.sampler, depth_view);
            self.shadow_pass.active = u.sh0[0] > 0.5;
        } else {
            self.rt = None;
            self.shadow_pass.active = false;
        }
        self.prep_ms.push(("tiles", t_tiles.elapsed().as_secs_f32() * 1000.));
        self.ao.fill_uniform(&mut u);
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));
        self.stats.meshes = self.mesh_count();
        self.stats.objects = self.objects.len();
        self.stats.atlas = self.layout.size;
    }
    pub fn ao_active(&self) -> bool { self.ao.active() }
    /// DF SHADOW SCREEN PASS: the half-resolution ray trace runs this frame (record [`Self::encode_shadow`] after the depth pre-pass + backdrop merge, before the scene pass).
    pub fn shadow_pass_active(&self) -> bool { self.shadow_pass.active }
    pub fn encode_shadow(&self, enc: &mut wgpu::CommandEncoder, ts: Option<wgpu::RenderPassTimestampWrites<'_>>) { self.shadow_pass.encode(enc, ts); }
    pub fn encode_ao(&self, enc: &mut wgpu::CommandEncoder, ts: Option<wgpu::ComputePassTimestampWrites<'_>>) { self.ao.encode(enc, ts); }
    /// PERF (debug only, `FN_DFAO_SPLIT=1`): [`crate::distance_field_ao::DfAo::encode_split`].
    pub fn encode_ao_split<'a>(&self, enc: &mut wgpu::CommandEncoder, ts: &dyn Fn(&str) -> Option<wgpu::ComputePassTimestampWrites<'a>>) { self.ao.encode_split(enc, ts); }
    pub fn end_frame(&mut self) { self.ao.end_frame(); }
    /// HISTORY RESET (2026-10-05): a game camera cut for the next frame's DFAO (`DfAo::camera_cut`); large moves are detected without it.
    pub fn camera_cut(&mut self) { self.ao.camera_cut(); }
    pub fn atlas_size(&self) -> [u32; 3] { self.layout.size }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_df(n: u32) -> MeshDistanceField {
        // a 100 cm cube centred at the origin, volume bounds +-150 cm: distance in volume space = (|p|_inf - 50) / 150
        let mut t = vec![];
        let (mn, mx) = (-1f32, 1f32);
        for z in 0..n { for y in 0..n { for x in 0..n {
            let p = (Vec3::new(x as f32, y as f32, z as f32) + 0.5) / n as f32 * 300. - 150.;
            let d = (p.abs().max_element() - 50.) / 150.;
            t.push((((d - mn) / (mx - mn)) * 255. + 0.5).floor().clamp(0., 255.) as u8);
        }}}
        MeshDistanceField { size: [n; 3], texels: t, box_min: [-150.; 3], box_max: [150.; 3], distance_min_max: [mn, mx], built_as_if_two_sided: false, mesh_was_plane: false, self_shadow_bias: 0., affects_distance_field_lighting: true }
    }

    /// FX SHADOW SELF-SHADOW BIAS: a copy that only changes the bias is the same volume (shares the atlas block); other texels are not.
    #[test]
    fn self_shadow_bias_variant_is_the_same_volume() {
        let a = cube_df(8);
        let mut b = a.clone();
        b.self_shadow_bias = 100.;
        assert!(a.same_volume(&b));
        let mut c = a.clone();
        c.texels[3] ^= 1;
        assert!(!a.same_volume(&c));
    }

    /// GDF PARTIAL UPDATES: an object-list rebuild reports the bounds of what was added, removed or moved (both bounds), nothing for unchanged objects.
    #[test]
    fn modified_object_bounds_are_the_added_removed_and_moved_objects() {
        let df = cube_df(8);
        let at = |x: f32| build_object(0, &df, [0; 3], [512, 512, 16], Mat4::from_translation(Vec3::new(x, 0., 0.))).unwrap();
        let old = vec![at(0.), at(1000.), at(2000.), at(2000.)];
        let new = vec![at(0.), at(2000.), at(3000.), at(1500.)];
        let m = modified_object_bounds(&old, &new);
        let xs: Vec<f32> = { let mut v: Vec<f32> = m.iter().map(|b| b.x).collect(); v.sort_by(f32::total_cmp); v };
        // added 3000 and 1500 (moved from 1000 / one of the 2000 pair), removed 1000 and one 2000
        assert_eq!(xs, vec![1000., 1500., 2000., 3000.]);
        assert!(m.iter().all(|b| (b.w - at(0.).radius).abs() < 1e-3));
        assert!(modified_object_bounds(&new, &new).is_empty());
    }

    #[test]
    fn instance_transform_matches_the_static_vertex_shader() {
        // static_common.wgsl: world = R_y(yaw) * (p * scale) + pos, with mesh-local p = UE local (x, z, y) * 0.01
        let inst = StaticMeshInstance::new(crate::static_mesh::StaticMeshHandle(0), Vec3::new(3., 0.5, -7.)).with_yaw(0.7).with_scale(1.3);
        let l2w = instance_local_to_world(&inst);
        let (c, s) = (inst.yaw.cos(), inst.yaw.sin());
        for ue in [Vec3::new(100., 0., 0.), Vec3::new(0., 100., 0.), Vec3::new(0., 0., 100.), Vec3::new(-37., 55., 210.)] {
            let p = Vec3::new(ue.x, ue.z, ue.y) * 0.01 * inst.scale;
            let world = Vec3::new(c * p.x + s * p.z, p.y, -s * p.x + c * p.z) + inst.pos;
            assert!((l2w.transform_point3(ue) - to_df(world)).length() < 1e-3, "{ue:?}");
        }
        // a proper rotation (no mirror): the DF volume keeps its handedness
        assert!(l2w.determinant() > 0.);
    }

    #[test]
    fn object_data_follows_process_primitive_update() {
        let df = cube_df(16);
        let l2w = Mat4::from_translation(Vec3::new(1000., 2000., 0.)) * Mat4::from_scale(Vec3::splat(2.));
        let o = build_object(0, &df, [32, 0, 0], [512, 512, 64], l2w).unwrap();
        // sphere: centre of the volume, radius = |Extent * scale|
        assert!((o.center - Vec3::new(1000., 2000., 0.)).length() < 1e-3);
        assert!((o.radius - (150f32 * 2.) * 3f32.sqrt()).abs() < 1e-2);
        // VolumeScale = MaxExtent * scale; LocalPositionExtent = 1 - 1/16
        assert!((o.signed_volume_scale - 300.).abs() < 1e-3);
        assert!((o.local_position_extent - Vec3::splat(1. - 1. / 16.)).length() < 1e-6);
        // the volume corner maps to the block's first texel corner and the centre to its middle: VolumeUV = p * UVScale + UVAdd
        let uv = |p: Vec3| p * o.uv_scale + o.uv_add;
        let c = uv(Vec3::ZERO);
        assert!((c - Vec3::new((32. + 8.) / 512., 8. / 512., 8. / 64.)).length() < 1e-6, "{c:?}");
        let lo = uv(-Vec3::ONE);
        assert!((lo - Vec3::new(32. / 512., 0., 0.)).length() < 1e-6, "{lo:?}");
        // WorldToVolume maps the box face to |p| = 1
        let v = o.world_to_volume.transform_point3(Vec3::new(1000. + 300., 2000., 0.));
        assert!((v - Vec3::new(1., 0., 0.)).length() < 1e-5, "{v:?}");
    }

    #[test]
    fn atlas_packing_keeps_blocks_disjoint() {
        let sizes: Vec<Option<[u32; 3]>> = (0..40).map(|i| Some([8 + (i * 7) % 57, 8 + (i * 13) % 57, 8 + (i * 5) % 57])).chain([None, Some([64, 64, 64])]).collect();
        let l = pack_atlas(&sizes);
        let boxes: Vec<([u32; 3], [u32; 3])> = sizes.iter().zip(&l.blocks).filter_map(|(s, b)| Some(((*b)?, (*s)?))).collect();
        assert_eq!(boxes.len(), 41);
        for (i, (a, sa)) in boxes.iter().enumerate() {
            for k in 0..3 { assert!(a[k] + sa[k] <= l.size[k]); }
            for (b, sb) in &boxes[i + 1..] {
                let overlap = (0..3).all(|k| a[k] < b[k] + sb[k] && b[k] < a[k] + sa[k]);
                assert!(!overlap);
            }
        }
    }

    fn view() -> DfView {
        let cam = CameraParams::from_yaw_pitch(Vec3::new(0., 1.7, 0.), 30., -5., 1280, 720);
        DfView::from_camera(&cam)
    }

    #[test]
    fn ray_traced_cascade_spans_csm_end_to_df_distance() {
        let v = view();
        let rt = rt_cascade(&v, dir_to_df(Vec3::new(0.3, 0.6, -0.2).normalize()), 2048, &lighting::CsmSetup::EPIC);
        assert!((rt.split_near - 4500.).abs() < 1e-2);
        assert_eq!(rt.split_far, 30000.);
        // previous split = near + 3/7 * (4500 - near) (exponent 2): fade region = 10 % of the last CSM cascade
        let s2 = v.near + 3. / 7. * (4500. - v.near);
        assert!((rt.split_near_fade_region - (4500. - s2) * 0.1).abs() < 1e-2);
        // tiles: W / 200 + 1, capped at 256
        assert_eq!(rt.tiles, ((rt.bounds_radius / 200. + 1.).min(256.)) as u32);
        // the sphere holds the sub-frustum: points on the near / far planes are inside
        for d in [rt.split_near, rt.split_far] {
            let p = v.origin + v.forward * d;
            assert!(p.distance(rt.bounds_center) <= rt.bounds_radius + 1e-2);
        }
    }

    #[test]
    fn shadow_space_maps_the_bounds_to_the_unit_square_and_depth_along_the_light() {
        let v = view();
        let to_light = dir_to_df(Vec3::new(0.3, 0.6, -0.2).normalize());
        let rt = rt_cascade(&v, to_light, 2048, &lighting::CsmSetup::EPIC);
        let c = rt.world_to_shadow.transform_point3(rt.bounds_center);
        assert!(c.x.abs() < 0.01 && c.y.abs() < 0.01, "{c:?}");
        // moving towards the light reduces the shadow depth (FaceDirection = the light's travel direction)
        let a = rt.world_to_shadow.transform_point3(rt.bounds_center + to_light * 100.);
        assert!(a.z < c.z);
        // xy only depend on the position perpendicular to the light
        let b = rt.world_to_shadow.transform_point3(rt.bounds_center + to_light * 777.);
        assert!((b.truncate() - c.truncate()).length() < 1e-4);
        // a point one radius away perpendicular to the light lands on the unit circle
        let perp = to_light.cross(Vec3::Z).normalize();
        let e = rt.world_to_shadow.transform_point3(rt.bounds_center + perp * rt.bounds_radius);
        assert!(((e.truncate() - c.truncate()).length() - 1.).abs() < 1e-3);
    }

    #[test]
    fn culling_hull_keeps_casters_between_the_sun_and_the_slice_and_rejects_the_rest() {
        let v = view();
        let to_light = dir_to_df(Vec3::new(0.2, 0.8, 0.1).normalize());
        let rt = rt_cascade(&v, to_light, 2048, &lighting::CsmSetup::EPIC);
        let inside = v.origin + v.forward * 10000.;
        assert!(hull_intersects_sphere(&rt.hull, inside, 100.));
        // a caster high up towards the sun from a receiver in the slice
        assert!(hull_intersects_sphere(&rt.hull, inside + to_light * 20000., 100.));
        // behind the camera and away from the light: culled
        assert!(!hull_intersects_sphere(&rt.hull, v.origin - v.forward * 20000. - to_light * 5000., 100.));
    }

    #[test]
    fn light_tile_of_a_receiver_lists_the_caster_above_it() {
        let v = view();
        let to_light = dir_to_df(Vec3::new(0.25, 0.9, -0.1).normalize());
        let rt = rt_cascade(&v, to_light, 2048, &lighting::CsmSetup::EPIC);
        let df = cube_df(16);
        let receiver = v.origin + v.forward * 9000. - Vec3::Z * v.origin.z;
        let caster_pos = receiver + to_light * 1500.;
        let l2w = Mat4::from_translation(caster_pos);
        let objs = vec![build_object(0, &df, [0; 3], [512, 512, 16], l2w).unwrap()];
        let tiles = cull_objects_to_light_tiles(&rt, &objs);
        assert_eq!(tiles.culled, 1);
        let (tx, ty) = light_tile_of(&rt, receiver).unwrap();
        let h = tiles.heads[(ty * tiles.dim + tx) as usize];
        assert!(h[1] >= 1, "receiver tile ({tx},{ty}) has no caster");
        // a receiver far to the side shares no tile with the caster
        let side = receiver + v.right * 8000.;
        if let Some((sx, sy)) = light_tile_of(&rt, side) { assert_eq!(tiles.heads[(sy * tiles.dim + sx) as usize][1], 0); }
    }
    #[test]
    fn threaded_light_tile_cull_equals_the_serial_lists() {
        // PERF: the worker split must reproduce the serial tile lists exactly (ascending object index per tile, capped)
        let v = view();
        let to_light = dir_to_df(Vec3::new(0.25, 0.9, -0.1).normalize());
        let rt = rt_cascade(&v, to_light, 2048, &lighting::CsmSetup::EPIC);
        let df = cube_df(16);
        let mut objs = vec![];
        for i in 0..600u32 {
            let p = v.origin + v.forward * (500. + (i % 37) as f32 * 300.) + v.right * (((i * 7919) % 41) as f32 * 200. - 4000.) - Vec3::Z * v.origin.z;
            objs.push(build_object(0, &df, [0; 3], [512, 512, 16], Mat4::from_translation(p) * Mat4::from_scale(Vec3::splat(1. + (i % 5) as f32))).unwrap());
        }
        let par = cull_objects_to_light_tiles(&rt, &objs);
        let (pairs, culled) = cull_object_range(&rt, &objs, 0);
        let mut lists: Vec<Vec<u32>> = vec![vec![]; (rt.tiles * rt.tiles) as usize];
        for (t, oi) in pairs { let l = &mut lists[t as usize]; if (l.len() as u32) < ue::MAX_OBJECTS_PER_CULL_TILE { l.push(oi); } }
        assert_eq!(par.culled, culled);
        assert!(par.indices.len() > 1000, "test scene too sparse: {}", par.indices.len());
        for (t, l) in lists.iter().enumerate() {
            let h = par.heads[t];
            assert_eq!(&par.indices[h[0] as usize..(h[0] + h[1]) as usize], l.as_slice(), "tile {t}");
        }
    }
    /// PERF: the per-tile form of the light-tile cull before the hoisting (exactness reference).
    fn cull_object_range_reference(rt: &RtCascade, objects: &[DfObject], first: usize) -> (Vec<(u32, u32)>, usize) {
        let n = rt.tiles;
        let mut out: Vec<(u32, u32)> = vec![];
        let w2s = rt.world_to_shadow;
        // the 4x4 stencil sphere, unit radius (ShadowRendering.h TStencilSphereVertexBuffer::InitRHI)
        let mut unit = vec![];
        let (sides, rings) = (ue::LOW_POLY_SPHERE_SIDES, ue::LOW_POLY_SPHERE_RINGS);
        for s in 0..=sides {
            let yaw = (360. * (s as f32 / sides as f32)).to_radians();
            let (sy, cy) = yaw.sin_cos();
            for v in 0..=rings {
                let a = v as f32 * std::f32::consts::PI / rings as f32;
                let arc = Vec3::new(0., a.sin(), a.cos());
                // FRotationMatrix(FRotator(0, yaw, 0)).TransformPosition: x*X + y*Y + z*Z with X = (cy, sy, 0), Y = (-sy, cy, 0), Z = (0, 0, 1)
                unit.push(Vec3::new(cy, sy, 0.) * arc.x + Vec3::new(-sy, cy, 0.) * arc.y + Vec3::Z * arc.z);
            }
        }
        // FShadowObjectCullVS::SetParameters: ConservativeRadiusScale = 1 / cos(PI / NumRings), MinRadius = 2 * ShadowRadius / min(NumGroups)
        let conservative = 1. / (std::f32::consts::PI / rings as f32).cos();
        let min_radius = 2. * rt.bounds_radius / n as f32;
        let nf = n as f32;
        let mut culled = 0usize;
        for (k, o) in objects.iter().enumerate() {
            let oi = first + k;
            if !hull_intersects_sphere(&rt.hull, o.center, o.radius) { continue; }
            culled += 1;
            // CulledObjectBoxBounds: the 8 local volume corners in shadow space
            let (lo, hi) = (o.local_min, o.local_max);
            let corners = [
                Vec3::new(lo.x, lo.y, lo.z), Vec3::new(hi.x, lo.y, lo.z), Vec3::new(lo.x, hi.y, lo.z), Vec3::new(hi.x, hi.y, lo.z),
                Vec3::new(lo.x, lo.y, hi.z), Vec3::new(hi.x, lo.y, hi.z), Vec3::new(lo.x, hi.y, hi.z), Vec3::new(hi.x, hi.y, hi.z),
            ];
            let vs: Vec<Vec3> = corners.iter().map(|c| w2s.transform_point3(o.local_to_world.transform_point3(*c))).collect();
            let (mut bmin, mut bmax) = (Vec3::splat(2e6), Vec3::splat(-2e6));
            for v in &vs { bmin = bmin.min(*v); bmax = bmax.max(*v); }
            let axis = |a: Vec3| a / a.dot(a).max(1e-4);
            let (axx, axy, axz) = (axis((vs[1] - vs[0]) * 0.5), axis((vs[2] - vs[0]) * 0.5), axis((vs[4] - vs[0]) * 0.5));
            let obj_center = 0.5 * (bmin + bmax);
            // rasterised footprint of the conservative sphere in light-tile pixels (D3D viewport: x = (cx * .5 + .5) * n, y = (1 - (cy * .5 + .5)) * n)
            let eff = (o.radius + min_radius) * conservative;
            let pts: Vec<Vec2> = unit.iter().map(|u| { let c = w2s.transform_point3(*u * eff + o.center); Vec2::new((c.x * 0.5 + 0.5) * nf, (1. - (c.y * 0.5 + 0.5)) * nf) }).collect();
            let hull2 = convex_hull(pts);
            if hull2.len() < 3 { continue; }
            let (mut pmin, mut pmax) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
            for p in &hull2 { pmin = pmin.min(*p); pmax = pmax.max(*p); }
            let x0 = (pmin.x - 0.5).ceil().max(0.) as i32;
            let x1 = ((pmax.x - 0.5).floor() as i32).min(n as i32 - 1);
            let y0 = (pmin.y - 0.5).ceil().max(0.) as i32;
            let y1 = ((pmax.y - 0.5).floor() as i32).min(n as i32 - 1);
            for ty in y0..=y1 {
                for tx in x0..=x1 {
                    if !inside_convex(&hull2, Vec2::new(tx as f32 + 0.5, ty as f32 + 0.5)) { continue; }
                    // ShadowObjectCullPS OBB_INTERSECTION
                    let tpc = Vec2::new(tx as f32, nf - ty as f32);
                    let tmin = Vec3::new((tpc.x - 1.) / nf * 2. - 1., (tpc.y - 1.) / nf * 2. - 1., -1000.);
                    let tmax = Vec3::new((tpc.x + 1.) / nf * 2. - 1., (tpc.y + 1.) / nf * 2. - 1., 1.);
                    if !(bmax.x > tmin.x && bmax.y > tmin.y && bmin.x < tmax.x && bmin.y < tmax.y && bmin.z < tmax.z) { continue; }
                    let (mut minp, mut maxp) = (Vec3::splat(500000.), Vec3::splat(-500000.));
                    for i in 0..8 {
                        let c = Vec3::new(if i & 1 == 0 { tmin.x } else { tmax.x }, if i & 2 == 0 { tmin.y } else { tmax.y }, if i & 4 == 0 { tmin.z } else { tmax.z });
                        let cv = c - obj_center;
                        let pr = Vec3::new(cv.dot(axx), cv.dot(axy), cv.dot(axz));
                        minp = minp.min(pr);
                        maxp = maxp.max(pr);
                    }
                    if minp.x < 1. && minp.y < 1. && minp.z < 1. && maxp.x > -1. && maxp.y > -1. && maxp.z > -1. {
                        out.push((ty as u32 * n + tx as u32, oi as u32));
                    }
                }
            }
        }
        (out, culled)
    }
    #[test]
    fn hoisted_light_tile_cull_equals_the_per_tile_form() {
        let v = view();
        let df = cube_df(16);
        for (li, dir) in [Vec3::new(0.25, 0.9, -0.1), Vec3::new(-0.6, 0.5, 0.3), Vec3::new(0.05, 1.0, 0.02)].iter().enumerate() {
            let to_light = dir_to_df(dir.normalize());
            let rt = rt_cascade(&v, to_light, 2048, &lighting::CsmSetup::EPIC);
            let mut objs: Vec<DfObject> = (0..400u32).map(|i| {
                let p = v.origin + v.forward * (300. + (i % 41) as f32 * 250.) + v.right * (((i * 7919 + li as u32) % 43) as f32 * 190. - 4000.) - Vec3::Z * v.origin.z;
                let rot = Mat4::from_rotation_z(i as f32 * 0.37) * Mat4::from_rotation_x((i % 3) as f32 * 0.2);
                build_object(0, &df, [0; 3], [512, 512, 16], Mat4::from_translation(p) * rot * Mat4::from_scale(Vec3::new(1. + (i % 5) as f32, 1. + (i % 3) as f32, 2.))).unwrap()
            }).collect();
            // PERF (2026-10-06): large footprints too (rocks / trees spanning dozens of light tiles), where the row-interval scan skips the most tiles
            objs.extend((0..40u32).map(|i| {
                let p = v.origin + v.forward * (800. + (i % 7) as f32 * 1500.) + v.right * ((i % 5) as f32 * 2500. - 5000.) - Vec3::Z * v.origin.z;
                let rot = Mat4::from_rotation_z(i as f32 * 1.1) * Mat4::from_rotation_y((i % 4) as f32 * 0.3);
                build_object(0, &df, [0; 3], [512, 512, 16], Mat4::from_translation(p) * rot * Mat4::from_scale(Vec3::new(15. + (i % 6) as f32 * 5., 20., 12. + (i % 3) as f32 * 8.))).unwrap()
            }));
            let t = std::time::Instant::now();
            let a = cull_object_range(&rt, &objs, 0);
            let ta = t.elapsed();
            let t = std::time::Instant::now();
            let b = cull_object_range_reference(&rt, &objs, 0);
            let tb = t.elapsed();
            eprintln!("light tile cull: hoisted {ta:?}, per-tile {tb:?}, {} entries", a.0.len());
            assert!(a.0.len() > 1000);
            assert_eq!(a, b);
        }
    }
    /// PERF (2026-10-08): the prepared cull (cached corners, grid-skipped cells, reused buffers, words written directly) must upload exactly the words of
    /// the old path, for scenes far larger than the cascade (most cells skipped), several sun directions and views, and with buffers reused between calls.
    #[test]
    fn prepared_light_tile_cull_gives_the_old_words_bit_for_bit() {
        let df = cube_df(16);
        // ~12k objects over 2 km x 2 km (DF space, cm) around the camera: small props, some huge ones, rotated / mirrored / scaled
        let mut objs: Vec<DfObject> = vec![];
        let mut s = 0x2468_ace1u32;
        let mut rnd = || { s ^= s << 13; s ^= s >> 17; s ^= s << 5; (s >> 8) as f32 / (1u32 << 24) as f32 };
        for i in 0..12000u32 {
            let p = Vec3::new(rnd() * 200000. - 100000., rnd() * 200000. - 100000., rnd() * 3000. - 500.);
            let big = i % 211 == 0;
            let sc = if big { Vec3::new(20. + rnd() * 30., 20. + rnd() * 30., 10. + rnd() * 20.) } else { Vec3::new(0.5 + rnd() * 6., 0.5 + rnd() * 6., 0.5 + rnd() * 8.) };
            let mirror = if i % 7 == 0 { Mat4::from_scale(Vec3::new(-1., 1., 1.)) } else { Mat4::IDENTITY };
            let l2w = Mat4::from_translation(p) * Mat4::from_rotation_z(rnd() * 6.3) * Mat4::from_rotation_x((rnd() - 0.5) * 0.4) * mirror * Mat4::from_scale(sc);
            objs.push(build_object(0, &df, [0; 3], [512, 512, 16], l2w).unwrap());
        }
        let mut prep = CullPrep::build(&objs);
        let mut sc = CullScratch::default();
        let mut words = vec![];
        let mut checked = 0;
        for (vi, (eye, yaw, pitch)) in [(Vec3::new(0., 1.7, 0.), 30., -5.), (Vec3::new(300., 40., -200.), 200., -30.), (Vec3::new(-900., 3., 950.), 95., 10.), (Vec3::new(2000., 1.7, 2000.), 0., -89.)].into_iter().enumerate() {
            let v = DfView::from_camera(&CameraParams::from_yaw_pitch(eye, yaw, pitch, 2544, 1353));
            for dir in [Vec3::new(0.25, 0.9, -0.1), Vec3::new(-0.6, 0.5, 0.3), Vec3::new(0.05, 1.0, 0.02), Vec3::new(0.7, 0.2, 0.68)] {
                let rt = rt_cascade(&v, dir_to_df(dir.normalize()), 2048, &lighting::CsmSetup::EPIC);
                let t = std::time::Instant::now();
                let old_tiles = cull_objects_to_light_tiles(&rt, &objs);
                let t_old = t.elapsed();
                let old = light_tile_words(&old_tiles);
                let t = std::time::Instant::now();
                let st = cull_light_tiles_prepared(&rt, &objs, &mut prep, &mut sc, &mut words);
                let t_new = t.elapsed();
                eprintln!("view {vi} sun {dir:?}: {} culled, {} entries; old {t_old:?}, prepared {t_new:?}", st.culled, st.entries);
                assert_eq!(st, LightTileStats { dim: old_tiles.dim, entries: old_tiles.indices.len(), culled: old_tiles.culled });
                assert_eq!(words, old, "view {vi} sun {dir:?}");
                if st.entries > 1000 { checked += 1; }
            }
        }
        assert!(checked >= 8, "test scenes too sparse ({checked})");
        assert!(prep.open.iter().filter(|o| !**o).count() > 0, "the grid skips cells");
    }
}
