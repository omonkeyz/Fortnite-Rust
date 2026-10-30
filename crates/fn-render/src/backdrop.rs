//! BACKDROP HOOK: everything of the 1.8 `Athena_Terrain` map that lies beyond the 512 m gameplay plate.
//!
//! How the real map is composed (RECOVERED from `Athena_Terrain.umap` + sublevels, see `fn-export props backdrop`):
//! * **Landscape**: 94 `LandscapeComponent`s (127 quads, 128x128 heightmap each, 200.787 cm per quad, Z scale 50 / 128 cm per raw unit, root at
//!   (-153000, -153000, -1692) cm) in the persistent level (2) and the always-loaded `Athena_Terrain_LS_00..05` sublevels; the lobby island sits in the
//!   far corner. Layer weights: Grass, GrassNoFoliage, Forest, Road_2, Mud (+ a data layer).
//! * **Ocean**: `SM_Athena_Water_Ocean` (1024 cm quad scaled 2000x2000x1) and `SM_OceanFar` (scaled 18.11) at z = -5312 cm, material `M_Athena_Water_Ocean`
//!   (opaque, roughness 0.5, `Water_Color` / `Water_Color_1`, `T_Water2_N` normals).
//! * **Far backdrop**: two `S_StoneLarge_01_Skybox` rocks and four `S_Terrace_01` terraces, 3-11 km out, `M_Skybox_Temperate_Terraces_01_moretiles(_Inst)`.
//! * The sky dome (`TODM_A_Child_Athena` StarMapMeshComponent, scale 37500x37500x18750) and the cloud dome belong to `sky.rs`, not here.
//!
//! This module draws them in their own pass between `sky` and `scene`: own projection (near 20 m, far 2e6 m, camera-relative vertices, the view's TAA jitter),
//! own depth target, so they never fight the 3 km main pass far plane. SSR BACKDROP (2026-10-05): that depth is then written into the scene depth converted to
//! the main projection (`encode_depth_merge`, backdrop_merge.wgsl; clamped just below 1 past the main far plane), because UE4.16 has one depth buffer: the
//! backdrop is geometry for the depth pre-pass / base pass depth test, HZB + SSR, AO, fog, TAA reprojection and the light-shaft / sun masks (it used to be cleared). World mapping = the props' (`x, y, z` UE cm -> `(x, z, y) * 0.01`
//! about the layout centre moved to the anchor); heights are re-based so the terrain height under the layout centre is y = 0 (the plate's level).
//!
//! NOT exact (labelled): the landscape / skybox / water materials' arithmetic nodes are not serialised in the cooked packages, so the shading here is a
//! reconstruction from their parameters + textures (layer tiling sizes, rock slope blend, water normal tiling and colour mix are ASSUMED).
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};
use image::RgbaImage;
use std::path::Path;

use crate::camera::CameraParams;
use crate::texture::{self, MipFilter};

/// Near / far of the backdrop pass (metres).
pub const BD_NEAR: f32 = 20.;
pub const BD_FAR: f32 = 2.0e6;
// TERRAIN MAIN PASS (2026-10-07): the CHOSEN `PLATE_BLEND_M` (450 m smoothstep fading the landscape from the flat plate's y = 0 to its real height) is gone:
// the real heights everywhere, the plate is no longer drawn when the landscape exists (renderer.rs set_backdrop).
/// PERF (2026-10-08): `FN_BACKDROP_FRUSTUM=0` turns off the side-plane cull of the backdrop pass (only the old "wholly behind the camera" test).
fn backdrop_frustum_cull() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_BACKDROP_FRUSTUM").map_or(true, |v| v != "0")) }
#[derive(Clone, Copy, Default, Debug)]
struct BackdropStats { draws: u64, tris: u64, main_draws: u64, main_tris: u64, occluded: u64 }
/// REPORT ONLY: (frames, summed [`BackdropStats`]).
static BD_RUN: std::sync::Mutex<(u64, BackdropStats)> = std::sync::Mutex::new((0, BackdropStats { draws: 0, tris: 0, main_draws: 0, main_tris: 0, occluded: 0 }));
static BD_LAST: std::sync::Mutex<BackdropStats> = std::sync::Mutex::new(BackdropStats { draws: 0, tris: 0, main_draws: 0, main_tris: 0, occluded: 0 });
pub fn cull_report() -> String {
    let Ok(g) = BD_LAST.lock() else { return String::new() };
    format!("terrain backdrop {} draws / {} tris, main {} draws / {} tris, {} occluded", g.draws, g.tris, g.main_draws, g.main_tris, g.occluded)
}
/// REPORT ONLY: run averages of the backdrop pass and the main-view landscape draws.
pub fn run_report() -> String {
    let Ok(g) = BD_RUN.lock() else { return String::new() };
    if g.0 == 0 { return String::new(); }
    let n = g.0 as f64;
    format!("backdrop run average ({} frames): backdrop pass {:.1} draws / {:.0} triangles, main-view landscape {:.1} draws / {:.0} triangles\n", g.0, g.1.draws as f64 / n, g.1.tris as f64 / n, g.1.main_draws as f64 / n, g.1.main_tris as f64 / n)
}
/// Depth (m) of the skirts that hide the cracks between landscape LODs.
const SKIRT_M: f32 = 12.;
/// Camera distance up to which each terrain LOD is used (strides 1, 4, 16 quads).
pub const LOD_STRIDES: [usize; 3] = [1, 4, 16];
pub const LOD_DIST_M: [f32; 3] = [450., 1600., f32::MAX];

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct BdVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Terrain = 0, Skybox = 1, Ocean = 2 }

/// A material: up to 6 textures (slot meaning depends on the kind) + 4 parameter vectors.
#[derive(Clone)]
pub struct BdMaterial {
    pub kind: Kind,
    pub textures: [Option<RgbaImage>; 6],
    /// SRGB BEFORE FILTER (2026-10-07): each texture's cooked `UTexture::SRGB` (backdrop.json `srgb`, written by the exporter from the Texture2D). UE samples an
    /// SRGB=true texture through an `_SRGB` view (D3D11RHIPrivate.h:912 `FindShaderResourceDXGIFormat`), so the hardware decodes to linear BEFORE filtering /
    /// mip blending; the slot is uploaded as `Rgba8UnormSrgb` (mips built in linear space, `build_mips_srgb`, like the cooker) and the shaders read linear values
    /// directly (was: UNORM upload + `to_linear` after filtering in WGSL, 0.4-5 % off on texture edges / minified tiles). Also decodes the skybox `Mask` AO
    /// texture, which is cooked SRGB=true (T_Skybox_Lowlands_Ambient_occlusion) and was read raw.
    pub srgb: [bool; 6],
    pub params: [[f32; 4]; 4],
}

pub struct BdMesh {
    pub name: String,
    pub verts: Vec<BdVertex>,
    pub indices: Vec<u32>,
    /// `(first_index, index_count)` per LOD (one entry for placed meshes).
    pub lods: Vec<(u32, u32)>,
    pub centre: Vec3,
    pub radius: f32,
    pub material: usize,
}

/// The landscape data types live in fn-terrain (shared with collision).
pub use fn_terrain::{parse_terrain, TerrainComp, TerrainSet};

/// A placed static mesh (`meshes/*.bin`, UE space) with its UE transform.
pub struct Placement {
    pub name: String,
    pub mesh: std::sync::Arc<MeshBin>,
    pub t: [f32; 3],
    pub q: [f32; 4],
    pub s: [f32; 3],
    pub material: usize,
}
pub struct MeshBin { pub verts: Vec<BdVertex>, pub indices: Vec<u32> }

pub struct BackdropInput {
    pub terrain: Option<TerrainSet>,
    pub placements: Vec<Placement>,
    pub materials: Vec<BdMaterial>,
    /// Material index of the terrain.
    pub terrain_material: usize,
    /// UE world position (cm) that maps to `anchor` (the props' layout centre).
    pub centre_cm: [f32; 3],
    pub anchor: Vec3,
    /// Half edge (m) of the gameplay plate (TERRAIN MAIN PASS: no longer cuts the landscape; the plate itself is only drawn without one).
    pub plate_half: f32,
    /// UE z (cm) of the ocean planes.
    pub sea_cm: f32,
}

pub struct BackdropData {
    pub meshes: Vec<BdMesh>,
    pub materials: Vec<BdMaterial>,
    pub plate_half: f32,
    /// Height (m) of the terrain under the layout centre in UE cm (the re-base).
    pub ground_ref_cm: f32,
    pub sea_y: f32,
    pub notes: Vec<String>,
    /// TERRAIN MAIN PASS: the landscape heights as one lattice (the global distance field heightfield), `None` without terrain.
    pub height_grid: Option<HeightGrid>,
}

// ------------------------------------------------------------------------------------------------------------------------------------
// file formats
// ------------------------------------------------------------------------------------------------------------------------------------
fn rd_u32(b: &[u8], p: &mut usize) -> Result<u32, String> {
    let v = b.get(*p..*p + 4).ok_or("truncated")?;
    *p += 4;
    Ok(u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
}
fn rd_f32(b: &[u8], p: &mut usize) -> Result<f32, String> { Ok(f32::from_bits(rd_u32(b, p)?)) }

/// `meshes/*.bin` written by `fn-export props backdrop-actors`.
pub fn parse_mesh(b: &[u8]) -> Result<MeshBin, String> {
    if b.len() < 16 || &b[..4] != b"FNBM" { return Err("mesh: bad magic".into()); }
    let mut p = 4;
    if rd_u32(b, &mut p)? != 1 { return Err("mesh: version".into()); }
    let (nv, ni) = (rd_u32(b, &mut p)? as usize, rd_u32(b, &mut p)? as usize);
    let mut verts = Vec::with_capacity(nv);
    for _ in 0..nv {
        let mut f = [0f32; 8];
        for x in f.iter_mut() { *x = rd_f32(b, &mut p)?; }
        let c = b.get(p..p + 4).ok_or("truncated")?;
        p += 4;
        verts.push(BdVertex { pos: [f[0], f[1], f[2]], normal: [f[3], f[4], f[5]], uv: [f[6], f[7]], color: [c[0], c[1], c[2], c[3]] });
    }
    let mut indices = Vec::with_capacity(ni);
    for _ in 0..ni { indices.push(rd_u32(b, &mut p)?); }
    Ok(MeshBin { verts, indices })
}

// ------------------------------------------------------------------------------------------------------------------------------------
// CPU geometry
// ------------------------------------------------------------------------------------------------------------------------------------
/// UE cm (x, y, z) -> game metres about `centre`, moved to `anchor`, heights re-based by `ref_z_cm`.
pub fn to_game(ue: [f32; 3], centre: [f32; 3], anchor: Vec3, ref_z_cm: f32) -> Vec3 {
    Vec3::new(anchor.x + (ue[0] - centre[0]) * 0.01, (ue[2] - ref_z_cm) * 0.01, anchor.z + (ue[1] - centre[1]) * 0.01)
}

/// Bilinear terrain height (UE cm) at a UE world xy (cm); `None` outside every component.
pub fn sample_height_cm(t: &TerrainSet, x: f32, y: f32) -> Option<f32> {
    for c in &t.comps {
        let (fx, fy) = ((x - c.origin[0]) / c.step[0], (y - c.origin[1]) / c.step[1]);
        let m = (c.size - 1) as f32;
        if fx < 0. || fy < 0. || fx > m || fy > m { continue; }
        let (x0, y0) = ((fx.floor() as usize).min(c.size - 2), (fy.floor() as usize).min(c.size - 2));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let h = |xx, yy| c.z_cm(xx, yy);
        return Some((h(x0, y0) * (1. - tx) + h(x0 + 1, y0) * tx) * (1. - ty) + (h(x0, y0 + 1) * (1. - tx) + h(x0 + 1, y0 + 1) * tx) * ty);
    }
    None
}

/// Layer weights of a vertex as (grass, forest, road, mud) in 0..255 (GrassNoFoliage counts as grass; a vertex without weights is grass).
fn splat(t: &TerrainSet, c: &TerrainComp, i: usize) -> [u8; 4] {
    let mut w = [0f32; 4];
    for (li, ws) in &c.layers {
        let slot = match t.layer_names.get(*li).map(String::as_str) {
            Some("Grass_LayerInfo") | Some("GrassNoFoliage_LayerInfo") => 0,
            Some("Forest_LayerInfo") => 1,
            Some("Road_2_LayerInfo") => 2,
            Some("Mud_LayerInfo") => 3,
            _ => continue,
        };
        w[slot] += ws[i] as f32;
    }
    let s: f32 = w.iter().sum();
    if s < 1. { return [255, 0, 0, 0]; }
    w.map(|v| (v / s * 255.).round() as u8)
}

/// Grid indices of one LOD: sample positions `0, s, 2s, .. , size-1`.
fn lod_axis(size: usize, stride: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..size - 1).step_by(stride).collect();
    v.push(size - 1);
    v
}

/// One component as a mesh with LODs (shared full-resolution vertex grid + skirt vertices appended per LOD).
pub fn build_terrain_mesh(t: &TerrainSet, ci: usize, centre: [f32; 3], anchor: Vec3, ref_z_cm: f32, material: usize) -> BdMesh {
    let c = &t.comps[ci];
    let n = c.size;
    // TERRAIN MAIN PASS (2026-10-07): the real height everywhere (was faded to the plate's y = 0 by PLATE_BLEND_M); the same mapping as fn_terrain's World
    let pos = |x: usize, y: usize| to_game([c.origin[0] + x as f32 * c.step[0], c.origin[1] + y as f32 * c.step[1], c.z_cm(x, y)], centre, anchor, ref_z_cm);
    let mut verts = Vec::with_capacity(n * n + 4 * n * 3);
    for y in 0..n {
        for x in 0..n {
            let (xm, xp, ym, yp) = (x.saturating_sub(1), (x + 1).min(n - 1), y.saturating_sub(1), (y + 1).min(n - 1));
            let dx = pos(xp, y) - pos(xm, y);
            let dz = pos(x, yp) - pos(x, ym);
            let nrm = dz.cross(dx).normalize_or_zero();
            let nrm = if nrm.y < 0. { -nrm } else { nrm };
            let w = splat(t, c, y * n + x);
            let p = pos(x, y);
            verts.push(BdVertex { pos: p.to_array(), normal: nrm.to_array(), uv: [0.; 2], color: w });
        }
    }
    let holes = fn_terrain::comp_holes(t, ci);
    let mut indices = vec![];
    let mut lods = vec![];
    for &stride in &LOD_STRIDES {
        let axis = lod_axis(n, stride);
        let first = indices.len() as u32;
        for ya in axis.windows(2) {
            for xa in axis.windows(2) {
                let id = |x: usize, y: usize| (y * n + x) as u32;
                if (ya[0]..ya[1]).any(|y| (xa[0]..xa[1]).any(|x| holes[y * n + x])) { continue; }
                let (a, b, cc, d) = (id(xa[0], ya[0]), id(xa[1], ya[0]), id(xa[1], ya[1]), id(xa[0], ya[1]));
                indices.extend_from_slice(&[a, d, cc, a, cc, b]);
            }
        }
        // skirts: duplicate the border samples lowered by SKIRT_M
        let mut skirt = |border: Vec<(usize, usize)>| {
            let base = verts.len() as u32;
            for &(x, y) in &border {
                let mut v = verts[y * n + x];
                v.pos[1] -= SKIRT_M;
                verts.push(v);
            }
            for i in 0..border.len() - 1 {
                let (a, b) = ((border[i].1 * n + border[i].0) as u32, (border[i + 1].1 * n + border[i + 1].0) as u32);
                let (sa, sb) = (base + i as u32, base + i as u32 + 1);
                indices.extend_from_slice(&[a, sa, sb, a, sb, b]);
            }
        };
        skirt(axis.iter().map(|&x| (x, 0)).collect());
        skirt(axis.iter().map(|&x| (x, n - 1)).collect());
        skirt(axis.iter().map(|&y| (0, y)).collect());
        skirt(axis.iter().map(|&y| (n - 1, y)).collect());
        lods.push((first, indices.len() as u32 - first));
    }
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for v in verts.iter().take(n * n) { lo = lo.min(Vec3::from(v.pos)); hi = hi.max(Vec3::from(v.pos)); }
    BdMesh { name: format!("terrain {ci}"), verts, indices, lods, centre: (lo + hi) * 0.5, radius: ((hi - lo) * 0.5).length() + SKIRT_M, material }
}

/// Bakes a placed mesh to game space.
pub fn bake_placement(p: &Placement, centre: [f32; 3], anchor: Vec3, ref_z_cm: f32) -> BdMesh {
    let q = Quat::from_xyzw(p.q[0], p.q[1], p.q[2], p.q[3]).normalize();
    let s = Vec3::from(p.s);
    let swap = |v: Vec3| Vec3::new(v.x, v.z, v.y);
    let mut verts = Vec::with_capacity(p.mesh.verts.len());
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for v in &p.mesh.verts {
        let w = q * (Vec3::from(v.pos) * s) + Vec3::from(p.t);
        let g = to_game(w.to_array(), centre, anchor, ref_z_cm);
        // normal: inverse-transpose of the scale, then rotation, then the axis swap
        let nn = q * (Vec3::from(v.normal) / s);
        let nn = swap(nn.normalize_or_zero());
        lo = lo.min(g);
        hi = hi.max(g);
        verts.push(BdVertex { pos: g.to_array(), normal: nn.to_array(), uv: v.uv, color: v.color });
    }
    let n = p.mesh.indices.len() as u32;
    BdMesh { name: p.name.clone(), verts, indices: p.mesh.indices.clone(), lods: vec![(0, n)], centre: (lo + hi) * 0.5, radius: ((hi - lo) * 0.5).length(), material: p.material }
}

/// `VertSnapGridSize` (384 cm): the height of a building level (fn-building `grid::VERT_SNAP_GRID_SIZE`).
pub const GRID_LEVEL_CM: f32 = 384.;

impl BackdropData {
    pub fn build(i: BackdropInput) -> BackdropData {
        let mut notes = vec![];
        let terrain_z = i.terrain.as_ref().and_then(|t| sample_height_cm(t, i.centre_cm[0], i.centre_cm[1])).unwrap_or(i.centre_cm[2]);
        // BUILDING CORE FIX 2: the flat plate stands in for the ground of a building level, so it sits on the structural grid's level
        // lattice (multiples of VertSnapGridSize 384 in absolute UE Z: the grid origin Z is -3840 = -10 x 384, FUN_1404f2160, and every
        // Athena_Terrain sublevel transform is on the 512 / 384 lattice). The level at or below the terrain under the layout centre is
        // taken (917 -> 768), so a ground floor's "WorldOverlapCheck" slab (+15 / -18) touches the plate as on the grid-aligned spawn island.
        let ground_ref = (terrain_z / GRID_LEVEL_CM).floor() * GRID_LEVEL_CM;
        notes.push(format!("ground reference: terrain height under the layout centre {terrain_z:.0} cm -> grid level {ground_ref:.0} cm (centre actor z {:.0})", i.centre_cm[2]));
        let mut meshes = vec![];
        if let Some(t) = &i.terrain {
            for ci in 0..t.comps.len() { meshes.push(build_terrain_mesh(t, ci, i.centre_cm, i.anchor, ground_ref, i.terrain_material)); }
        }
        for p in &i.placements { meshes.push(bake_placement(p, i.centre_cm, i.anchor, ground_ref)); }
        let height_grid = i.terrain.as_ref().and_then(|t| height_grid(t, i.centre_cm, i.anchor, ground_ref));
        BackdropData { meshes, materials: i.materials, plate_half: i.plate_half, ground_ref_cm: ground_ref, sea_y: (i.sea_cm - ground_ref) * 0.01, notes, height_grid }
    }
    /// TERRAIN MAIN PASS: true when the landscape is part of the data (the flat plate is then not drawn).
    pub fn has_terrain(&self) -> bool { self.materials.iter().enumerate().any(|(mi, m)| m.kind == Kind::Terrain && self.meshes.iter().any(|x| x.material == mi)) }
}

/// TERRAIN MAIN PASS (2026-10-07): the whole landscape as ONE height lattice in game space, for the global distance field's heightfield (distance_field_ao.rs, UE4.16
/// CompositeHeightfieldsIntoGlobalDistanceFieldCS composites the landscape there). Sample (i, j) lies at game (x, z) = `origin + (i, j) * cell`; `y` = game height (m),
/// [`HEIGHT_HOLE`] where no component covers the lattice point (UE's MinMaxHeightfieldUV test skips those).
pub struct HeightGrid { pub w: u32, pub h: u32, pub origin: [f32; 2], pub cell: f32, pub y: Vec<f32> }
pub const HEIGHT_HOLE: f32 = -1.0e7;
/// The landscape components' samples on their shared lattice (neighbouring components repeat their border row: the same value is written twice).
pub fn height_grid(t: &TerrainSet, centre: [f32; 3], anchor: Vec3, ref_z_cm: f32) -> Option<HeightGrid> {
    let step = t.comps.first()?.step[0];
    if step <= 0. || t.comps.iter().any(|c| (c.step[0] - step).abs() > 1e-3 || (c.step[1] - step).abs() > 1e-3) { return None; } // ASSUMED: one uniform lattice (true for Athena)
    let lo = [t.comps.iter().map(|c| c.origin[0]).fold(f32::MAX, f32::min), t.comps.iter().map(|c| c.origin[1]).fold(f32::MAX, f32::min)];
    let cells = |c: &TerrainComp, a: usize| ((c.origin[a] - lo[a]) / step).round() as usize;
    let w = t.comps.iter().map(|c| cells(c, 0) + c.size).max()?;
    let h = t.comps.iter().map(|c| cells(c, 1) + c.size).max()?;
    let mut y = vec![HEIGHT_HOLE; w * h];
    for c in &t.comps {
        let (ox, oy) = (cells(c, 0), cells(c, 1));
        for j in 0..c.size { for i in 0..c.size { y[(oy + j) * w + ox + i] = (c.z_cm(i, j) - ref_z_cm) * 0.01; } }
    }
    let o = to_game([lo[0], lo[1], 0.], centre, anchor, ref_z_cm);
    Some(HeightGrid { w: w as u32, h: h as u32, origin: [o.x, o.z], cell: step * 0.01, y })
}

// ------------------------------------------------------------------------------------------------------------------------------------
// GPU
// ------------------------------------------------------------------------------------------------------------------------------------
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BdUniform { view_rot_proj: [[f32; 4]; 4], eye: [f32; 4], misc: [f32; 4] }
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MatUniform { kind: [u32; 4], p: [[f32; 4]; 4] }

struct GpuMesh { vb: wgpu::Buffer, ib: wgpu::Buffer, vr: std::ops::Range<u64>, ir: std::ops::Range<u64>, lods: Vec<(u32, u32)>, centre: Vec3, radius: f32, material: usize, terrain: bool }

pub struct BackdropRenderer {
    pipeline: wgpu::RenderPipeline,
    /// SSR BACKDROP (2026-10-05): the pass's own depth (own near / far) and the full-screen pass that writes it into the scene depth, see [`Self::encode_depth_merge`]
    merge: wgpu::RenderPipeline,
    merge_bgl: wgpu::BindGroupLayout,
    depth: Option<((u32, u32), wgpu::TextureView, wgpu::BindGroup)>,
    /// PERF (2026-10-06): group 2 of the pass = the scene depth of the pre-pass (backdrop.wgsl `covered_by_scene`), keyed by the renderer's target generation.
    scene_bgl: wgpu::BindGroupLayout,
    scene_bg: Option<(u64, wgpu::BindGroup)>,
    uniform: wgpu::Buffer,
    material_bgs: Vec<wgpu::BindGroup>,
    meshes: Vec<GpuMesh>,
    /// Camera position of the last `prepare` (LOD selection).
    eye: Vec3,
    /// Per mesh: LOD chosen this frame for THIS pass, `None` = culled or drawn by the main pass.
    chosen: Vec<Option<usize>>,
    /// TERRAIN MAIN PASS (2026-10-07): the landscape's main-pass pipelines (None without terrain); per frame the (mesh, LOD) of the main view and of each
    /// cascade's casters, see [`Self::prepare`].
    main: Option<MainTerrain>,
    main_view: Vec<(usize, usize)>,
    main_shadow: Vec<Vec<(usize, usize)>>,
    /// LANDSCAPE MATERIAL (2026-10-07): the landscape components within the main pass are drawn by the FX path on their cooked material (fn-app
    /// landscape_render.rs): this pass then draws neither their depth pre-pass nor their lit base pass (the CSM casters and the far backdrop stay).
    pub suppress_main: bool,
    layouts: (wgpu::PipelineLayout, wgpu::PipelineLayout, Option<(wgpu::PipelineLayout, wgpu::PipelineLayout)>),
    formats: (wgpu::TextureFormat, wgpu::TextureFormat),
}

/// TERRAIN MAIN PASS (2026-10-07): depth pre-pass twin + lit base pass (terrain.wgsl, one module / one vertex entry) and the CSM caster pass (shadow.wgsl vs_prop).
struct MainTerrain { depth: wgpu::RenderPipeline, lit: wgpu::RenderPipeline, shadow: wgpu::RenderPipeline }

const ATTRS: [wgpu::VertexAttribute; 4] = [
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 24, shader_location: 2 },
    wgpu::VertexAttribute { format: wgpu::VertexFormat::Unorm8x4, offset: 32, shader_location: 3 },
];

fn backdrop_pipelines(device: &wgpu::Device, layout: &wgpu::PipelineLayout, merge_layout: &wgpu::PipelineLayout, terrain: Option<&(wgpu::PipelineLayout, wgpu::PipelineLayout)>, hdr: wgpu::TextureFormat, depth: wgpu::TextureFormat) -> (wgpu::RenderPipeline, wgpu::RenderPipeline, Option<MainTerrain>) {
    let src = [fn_live::wgsl!("../shaders/common.wgsl") /* TOD LIVE: the fog is per-frame uniform data (the globals light block, LightUniform::fog) */, fn_live::wgsl!("../shaders/lit_common.wgsl"), fn_live::wgsl!("../shaders/terrain_common.wgsl") /* TERRAIN MAIN PASS */, fn_live::wgsl!("../shaders/backdrop.wgsl")].join("\n");
    let sm = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("backdrop"), source: wgpu::ShaderSource::Wgsl(src.into()) });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("backdrop"), layout: Some(layout),
        vertex: wgpu::VertexState { module: &sm, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<BdVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &ATTRS })] },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState { format: depth, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: Default::default() }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState { module: &sm, entry_point: Some("fs_main"), compilation_options: Default::default(), targets: &crate::reflection::lit_targets(hdr) }), // REFLECTION APPLY: + reflection inputs
        multiview_mask: None, cache: None,
    });
    let merge_sm = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("backdrop depth merge"), source: wgpu::ShaderSource::Wgsl(fn_live::wgsl!("../shaders/backdrop_merge.wgsl").into()) });
    let merge = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("backdrop depth merge"), layout: Some(merge_layout),
        vertex: wgpu::VertexState { module: &merge_sm, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
        primitive: Default::default(),
        depth_stencil: Some(wgpu::DepthStencilState { format: depth, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less) /* PERF: the scene depth now holds the pre-pass: the nearer depth wins */, stencil: Default::default(), bias: Default::default() }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState { module: &merge_sm, entry_point: Some("fs_main"), compilation_options: Default::default(), targets: &[] }),
        multiview_mask: None, cache: None,
    });
    let main = terrain.map(|(tl, sl)| {
        let tsm = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("terrain"), source: wgpu::ShaderSource::Wgsl([fn_live::wgsl!("../shaders/common.wgsl"), fn_live::wgsl!("../shaders/lit_common.wgsl"),
            fn_live::wgsl!("../shaders/terrain_common.wgsl"), fn_live::wgsl!("../shaders/terrain.wgsl")].join("\n").into()) });
        let vbl = [Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<BdVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &ATTRS })];
        let ds = wgpu::DepthStencilState { format: depth, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: Default::default() };
        let prim = wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None /* the skirts face both ways */, ..Default::default() };
        let pipe = |label: &str, ds: wgpu::DepthStencilState, fs: Option<wgpu::FragmentState>| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label), layout: Some(tl), vertex: wgpu::VertexState { module: &tsm, entry_point: Some("vs_terrain"), compilation_options: Default::default(), buffers: &vbl },
            primitive: prim, depth_stencil: Some(ds), multisample: Default::default(), fragment: fs, multiview_mask: None, cache: None,
        });
        let targets = crate::reflection::lit_targets(hdr);
        let depth_p = pipe("terrain depth", ds.clone(), None);
        let lit = pipe("terrain", wgpu::DepthStencilState { depth_compare: Some(wgpu::CompareFunction::Equal), depth_write_enabled: Some(false), ..ds.clone() },
            Some(wgpu::FragmentState { module: &tsm, entry_point: Some("fs_terrain"), compilation_options: Default::default(), targets: &targets }));
        let ssm = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("terrain shadow"), source: wgpu::ShaderSource::Wgsl(fn_live::wgsl!("../shaders/shadow.wgsl").into()) });
        let shadow = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain shadow"), layout: Some(sl),
            vertex: wgpu::VertexState { module: &ssm, entry_point: Some("vs_prop"), compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<BdVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &ATTRS[..1] })] },
            // UE4.16: no hardware depth bias in the depth pass (ShaderDepthBias is added in shadow.wgsl), as the renderer's other caster pipelines
            primitive: prim, depth_stencil: Some(ds), multisample: Default::default(), fragment: None, multiview_mask: None, cache: None,
        });
        MainTerrain { depth: depth_p, lit, shadow }
    });
    (pipeline, merge, main)
}

pub struct BackdropPrep { chains: std::collections::HashMap<(usize, usize), Vec<RgbaImage>>, verts: Vec<BdVertex>, indices: Vec<u32>, ranges: Vec<(std::ops::Range<u64>, std::ops::Range<u64>)> }

pub fn prepare(data: &BackdropData) -> BackdropPrep {
    let jobs: Vec<(usize, usize, &RgbaImage, bool)> = data.materials.iter().enumerate().flat_map(|(mi, m)| m.textures.iter().zip(m.srgb).enumerate().filter_map(move |(ti, (t, s))| t.as_ref().map(|img| (mi, ti, img, s)))).collect();
    let chains: std::collections::HashMap<(usize, usize), Vec<RgbaImage>> = std::thread::scope(|sc| {
        let hs: Vec<_> = jobs.iter().map(|&(mi, ti, img, s)| sc.spawn(move || ((mi, ti), texture::mip_chain(img, s, MipFilter::Triangle)))).collect();
        hs.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let (mut verts, mut indices): (Vec<BdVertex>, Vec<u32>) = (vec![], vec![]);
    let mut ranges = vec![];
    for m in &data.meshes {
        let vs = std::mem::size_of::<BdVertex>() as u64;
        let (v0, i0) = (verts.len() as u64 * vs, indices.len() as u64 * 4);
        verts.extend_from_slice(&m.verts);
        indices.extend_from_slice(&m.indices);
        ranges.push((v0..verts.len() as u64 * vs, i0..indices.len() as u64 * 4));
    }
    if verts.is_empty() { verts.push(BdVertex::zeroed()); }
    if indices.is_empty() { indices.push(0); }
    BackdropPrep { chains, verts, indices, ranges }
}

impl BackdropRenderer {
    /// `bgl_shadow` = the cascades' LightViewProj layout (shadow.wgsl group 0), for the main-pass terrain casters.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, bgl_globals: &wgpu::BindGroupLayout, bgl_shadow: &wgpu::BindGroupLayout, hdr: wgpu::TextureFormat, depth: wgpu::TextureFormat, data: &BackdropData) -> BackdropRenderer {
        Self::new_prepared(device, queue, bgl_globals, bgl_shadow, hdr, depth, data, prepare(data), false)
    }
    pub fn new_prepared(device: &wgpu::Device, queue: &wgpu::Queue, bgl_globals: &wgpu::BindGroupLayout, bgl_shadow: &wgpu::BindGroupLayout, hdr: wgpu::TextureFormat, depth: wgpu::TextureFormat, data: &BackdropData, prep: BackdropPrep, deferred: bool) -> BackdropRenderer {
        let v = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let f = wgpu::ShaderStages::FRAGMENT;
        let mut entries = vec![
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: v, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: f, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
        ];
        for i in 0..6u32 {
            entries.push(wgpu::BindGroupLayoutEntry { binding: 2 + i, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None });
        }
        entries.push(wgpu::BindGroupLayoutEntry { binding: 8, visibility: f, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("backdrop bgl"), entries: &entries });
        // PERF (2026-10-06): group 2 = the scene depth (texture_depth_2d, read with textureLoad)
        let scene_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("backdrop scene depth bgl"), entries: &[
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
        ] });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("backdrop layout"), bind_group_layouts: &[Some(bgl_globals), Some(&bgl), Some(&scene_bgl)], immediate_size: 0 });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor { label: Some("backdrop uniform"), size: std::mem::size_of::<BdUniform>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let sampler = texture::repeat_sampler(device, 8, "backdrop sampler");
        let white = texture::solid_rgba8(device, queue, [255, 255, 255, 255], "backdrop white");
        let wview = white.create_view(&Default::default());
        let mut material_bgs = vec![];
        let BackdropPrep { mut chains, verts: all_v, indices: all_i, ranges } = prep;
        for (mi, m) in data.materials.iter().enumerate() {
            let mu = MatUniform { kind: [m.kind as u32, 0, 0, 0], p: m.params };
            let mbuf = texture::buffer_init(device, &[mu], wgpu::BufferUsages::UNIFORM, "backdrop material");
            let views: Vec<wgpu::TextureView> = m.textures.iter().zip(m.srgb).enumerate().map(|(ti, (t, srgb))| match t {
                Some(img) => match chains.remove(&(mi, ti)) { Some(l) if deferred => texture::upload_mip_chain_deferred(device, l, srgb, &format!("backdrop m{mi}")), Some(l) => texture::upload_mip_chain(device, queue, img, &l, srgb, &format!("backdrop m{mi}")), None => texture::upload_rgba8(device, queue, img, srgb /* SRGB BEFORE FILTER */, MipFilter::Triangle, &format!("backdrop m{mi}")) }.create_view(&Default::default()),
                None => wview.clone(),
            }).collect();
            let mut be = vec![wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }, wgpu::BindGroupEntry { binding: 1, resource: mbuf.as_entire_binding() }];
            for (i, vw) in views.iter().enumerate() { be.push(wgpu::BindGroupEntry { binding: 2 + i as u32, resource: wgpu::BindingResource::TextureView(vw) }); }
            be.push(wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::Sampler(&sampler) });
            material_bgs.push(device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("backdrop material bg"), layout: &bgl, entries: &be }));
        }
        let (shared_vb, shared_ib) = if deferred { (texture::buffer_deferred(device, &all_v, wgpu::BufferUsages::VERTEX, "terrain backdrop vertices"), texture::buffer_deferred(device, &all_i, wgpu::BufferUsages::INDEX, "terrain backdrop indices")) }
            else { (texture::buffer_init(device, &all_v, wgpu::BufferUsages::VERTEX, "terrain backdrop vertices"), texture::buffer_init(device, &all_i, wgpu::BufferUsages::INDEX, "terrain backdrop indices")) };
        drop((all_v, all_i));
        let meshes: Vec<GpuMesh> = data.meshes.iter().zip(ranges).map(|(m, (vr, ir))| GpuMesh {
            vb: shared_vb.clone(), ib: shared_ib.clone(), vr, ir,
            lods: m.lods.clone(), centre: m.centre, radius: m.radius, material: m.material, terrain: data.materials.get(m.material).map_or(false, |x| x.kind == Kind::Terrain),
        }).collect();
        let n = meshes.len();
        // SSR BACKDROP: depth-only full-screen pass converting the backdrop depth into the scene depth (backdrop_merge.wgsl)
        let merge_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("backdrop depth merge bgl"), entries: &[
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: f, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: f, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
        ] });
        let merge_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("backdrop depth merge layout"), bind_group_layouts: &[Some(&merge_bgl)], immediate_size: 0 });
        // TERRAIN MAIN PASS (2026-10-07): the landscape in the main pass. Layout = globals + this module's material group (same bind groups; `bd` unread); the
        // pre-pass twin writes the depth (Less, as the plate's), the lit pipeline tests Equal without depth writes (UE4.16 full pre-pass, renderer.rs FULL PREPASS);
        // the casters render with the cascades' LightViewProj (shadow.wgsl vs_prop reads the position only) like the other whole-scene CSM casters.
        let terrain_layouts = meshes.iter().any(|m| m.terrain).then(|| (device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("terrain layout"), bind_group_layouts: &[Some(bgl_globals), Some(&bgl)], immediate_size: 0 }), device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("terrain shadow layout"), bind_group_layouts: &[Some(bgl_shadow)], immediate_size: 0 })));
        let (pipeline, merge, main) = backdrop_pipelines(device, &layout, &merge_layout, terrain_layouts.as_ref(), hdr, depth);
        BackdropRenderer { pipeline, merge, merge_bgl, depth: None, scene_bgl, scene_bg: None, uniform, material_bgs, meshes, eye: Vec3::ZERO, chosen: vec![None; n], main, main_view: vec![], main_shadow: vec![], suppress_main: false, layouts: (layout, merge_layout, terrain_layouts), formats: (hdr, depth) }
    }

    pub fn rebuild_pipelines(&mut self, device: &wgpu::Device) -> Result<(), String> {
        let (l, m, t) = &self.layouts;
        let (pipeline, merge, main) = crate::renderer::checked_build(device, || backdrop_pipelines(device, l, m, t.as_ref(), self.formats.0, self.formats.1))?;
        (self.pipeline, self.merge, self.main) = (pipeline, merge, main);
        Ok(())
    }

    /// Per-frame uniforms + LOD choice. TERRAIN MAIN PASS (2026-10-07): `cascades` = this frame's shadow-mapped cascades (empty / `shadows_on` false: no casters).
    /// Every landscape component is drawn by ONE pass of the view: the main pass when its bounding sphere lies wholly within the main far plane, else this backdrop
    /// pass (never both: their depths would tie and the lit main pass's Equal test would pick either), so the near landscape gets the scene's CSM / DF shadows.
    /// The casters are every component meeting a cascade (`Cascade::caster_in_bounds`), at the LOD of the view (a coarser caster than its receiver self-shadows).
    pub fn prepare(&mut self, queue: &wgpu::Queue, cam: &CameraParams, time: f32, cascades: &[crate::lighting::Cascade], shadows_on: bool) {
        let (_, ty) = cam.tan_half();
        // SSR BACKDROP: the view's temporal AA jitter too (UE jitters every primitive of the view; the merged depth then matches the jittered scene depth)
        let proj = Mat4::from_translation(Vec3::new(cam.jitter[0], cam.jitter[1], 0.)) * Mat4::perspective_rh(2. * ty.atan(), cam.aspect(), BD_NEAR, BD_FAR);
        let view = Mat4::look_to_rh(Vec3::ZERO, cam.forward, cam.up);
        // misc: x unused (was the plate half edge), y / z the main pass near / far (m) the depth merge converts to
        let u = BdUniform { view_rot_proj: (proj * view).to_cols_array_2d(), eye: [cam.eye.x, cam.eye.y, cam.eye.z, time], misc: [0., cam.near, cam.far, 0.] };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));
        self.eye = cam.eye;
        let fwd = cam.forward;
        let main_on = self.main.is_some();
        let planes = crate::static_mesh::frustum_planes(&(cam.proj_matrix() * cam.view_matrix()));
        self.main_view.clear();
        self.main_shadow.resize_with(cascades.len(), Vec::new);
        for l in &mut self.main_shadow { l.clear(); }
        let side_cull = backdrop_frustum_cull();
        let mut st = BackdropStats::default();
        for (i, m) in self.meshes.iter().enumerate() {
            let d = m.centre - cam.eye;
            let dist = d.length();
            // cull spheres that are entirely behind the camera (cheap; the GPU clips the rest)
            // PERF (2026-10-08, FN_BACKDROP_FRUSTUM=0 = off): and spheres wholly outside one of the 4 side planes of the view (the backdrop projection has the
            // main view's fov / aspect, only its near / far differ), grown by 0.2 % of the distance (about 3 px at 2544 wide: covers the TAA jitter). Such a
            // mesh covers no pixel, so the image is the same; it was drawn whole (far landscape components at LOD 0 / 1 to the sides and behind the view's
            // side planes) and clipped by the GPU.
            let behind = (d.dot(fwd) < -m.radius && dist > m.radius * 1.01)
                || (side_cull && !crate::static_mesh::sphere_in_planes(&planes, 4, m.centre, m.radius + dist * 0.002));
            if !m.terrain { self.chosen[i] = if behind { None } else { Some(0) }; continue; }
            let near = (dist - m.radius).max(0.);
            let lod = LOD_DIST_M.iter().position(|&l| near < l).unwrap_or(LOD_DIST_M.len() - 1);
            if main_on && shadows_on {
                for (c, cas) in cascades.iter().enumerate() { if cas.caster_in_bounds(m.centre, m.radius) { self.main_shadow[c].push((i, lod)); } }
            }
            if main_on && dist + m.radius < cam.far {
                self.chosen[i] = None;
                if crate::static_mesh::sphere_in_planes(&planes, 6, m.centre, m.radius) && !self.suppress_main { self.main_view.push((i, lod)); }
            } else {
                self.chosen[i] = if behind { None } else { Some(lod) };
            }
        }
        // REPORT ONLY: this frame's backdrop-pass draws / triangles and main-view landscape draws / triangles, summed over the run
        for (m, l) in self.meshes.iter().zip(&self.chosen) { if let Some(l) = l { st.draws += 1; st.tris += m.lods[(*l).min(m.lods.len() - 1)].1 as u64 / 3; } }
        let (mv, mt) = self.main_stats();
        st.main_draws = mv as u64; st.main_tris = mt;
        if let Ok(mut g) = BD_RUN.lock() { g.0 += 1; g.1.draws += st.draws; g.1.tris += st.tris; g.1.main_draws += st.main_draws; g.1.main_tris += st.main_tris; }
        if let Ok(mut g) = BD_LAST.lock() { *g = st; }
    }

    /// TERRAIN MAIN PASS (2026-10-07): the landscape is drawn by the main pass (the renderer then drops the flat plate).
    pub fn has_main_terrain(&self) -> bool { self.main.is_some() }
    /// LANDSCAPE MATERIAL (2026-10-07): (centre, radius) in game metres of every landscape component mesh, in the terrain component order (the main pass
    /// takes a component when `distance + radius < far`, [`Self::prepare`]).
    pub fn terrain_bounds(&self) -> Vec<(Vec3, f32)> { self.meshes.iter().filter(|m| m.terrain).map(|m| (m.centre, m.radius)).collect() }
    /// TERRAIN MAIN PASS: (components, triangles) of the main view this frame (report only).
    pub fn main_stats(&self) -> (usize, u64) { (self.main_view.len(), self.main_view.iter().map(|&(i, l)| self.meshes[i].lods[l.min(self.meshes[i].lods.len() - 1)].1 as u64 / 3).sum()) }
    fn draw_list(&self, pass: &mut wgpu::RenderPass<'_>, list: &[(usize, usize)]) {
        let mut cur = usize::MAX;
        for &(i, l) in list {
            let m = &self.meshes[i];
            if cur != m.material { pass.set_bind_group(1, &self.material_bgs[m.material], &[]); cur = m.material; }
            let (first, count) = m.lods[l.min(m.lods.len() - 1)];
            pass.set_vertex_buffer(0, m.vb.slice(m.vr.clone()));
            pass.set_index_buffer(m.ib.slice(m.ir.clone()), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(first..first + count, 0, 0..1);
        }
    }
    /// TERRAIN MAIN PASS: the depth pre-pass draws (sets its pipeline and groups 0 / 1).
    pub fn draw_main_depth(&self, pass: &mut wgpu::RenderPass<'_>, globals: &wgpu::BindGroup) {
        let Some(t) = &self.main else { return };
        if self.main_view.is_empty() { return; }
        pass.set_pipeline(&t.depth);
        pass.set_bind_group(0, globals, &[]);
        self.draw_list(pass, &self.main_view);
    }
    /// TERRAIN MAIN PASS: the lit base-pass draws (depth Equal against the pre-pass; sets its pipeline and groups 0 / 1).
    pub fn draw_main(&self, pass: &mut wgpu::RenderPass<'_>, globals: &wgpu::BindGroup) {
        let Some(t) = &self.main else { return };
        if self.main_view.is_empty() { return; }
        pass.set_pipeline(&t.lit);
        pass.set_bind_group(0, globals, &[]);
        self.draw_list(pass, &self.main_view);
    }
    /// TERRAIN MAIN PASS: cascade `c`'s landscape casters (`vp` = its LightViewProj bind group; sets its pipeline and group 0).
    pub fn draw_main_shadow(&self, pass: &mut wgpu::RenderPass<'_>, c: usize, vp: &wgpu::BindGroup) {
        let (Some(t), Some(list)) = (&self.main, self.main_shadow.get(c)) else { return };
        if list.is_empty() { return; }
        pass.set_pipeline(&t.shadow);
        pass.set_bind_group(0, vp, &[]);
        for &(i, l) in list {
            let m = &self.meshes[i];
            let (first, count) = m.lods[l.min(m.lods.len() - 1)];
            pass.set_vertex_buffer(0, m.vb.slice(m.vr.clone()));
            pass.set_index_buffer(m.ib.slice(m.ir.clone()), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(first..first + count, 0, 0..1);
        }
    }

    /// PERF (2026-10-06): binds the scene depth `view` (the pre-pass target; `key` = its generation) as group 2 of the pass (backdrop.wgsl `covered_by_scene`).
    pub fn ensure_scene_depth(&mut self, device: &wgpu::Device, view: &wgpu::TextureView, key: u64) {
        if self.scene_bg.as_ref().map(|b| b.0) == Some(key) { return; }
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("backdrop scene depth bg"), layout: &self.scene_bgl, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
        ] });
        self.scene_bg = Some((key, bg));
    }
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, globals: &wgpu::BindGroup) {
        let Some((_, scene_bg)) = &self.scene_bg else { return }; // PERF: needs the scene depth (ensure_scene_depth)
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, globals, &[]);
        pass.set_bind_group(2, scene_bg, &[]);
        let mut cur = usize::MAX;
        for (m, lod) in self.meshes.iter().zip(&self.chosen) {
            let Some(l) = lod else { continue };
            if m.material != cur { pass.set_bind_group(1, &self.material_bgs[m.material], &[]); cur = m.material; }
            let (first, count) = m.lods[(*l).min(m.lods.len() - 1)];
            pass.set_vertex_buffer(0, m.vb.slice(m.vr.clone()));
            pass.set_index_buffer(m.ib.slice(m.ir.clone()), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(first..first + count, 0, 0..1);
        }
    }
    pub fn mesh_count(&self) -> usize { self.meshes.len() }

    /// SSR BACKDROP (2026-10-05): (re)creates the backdrop pass's own depth target for a `w` x `h` view (call before recording the frame).
    pub fn ensure_depth(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if self.depth.as_ref().map(|d| d.0) == Some((w, h)) { return; }
        let view = device.create_texture(&wgpu::TextureDescriptor { label: Some("backdrop depth"), size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Depth32Float, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] }).create_view(&Default::default());
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("backdrop depth merge bg"), layout: &self.merge_bgl, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: self.uniform.as_entire_binding() },
        ] });
        self.depth = Some(((w, h), view, bg));
    }
    /// The backdrop pass's own depth (near [`BD_NEAR`], far [`BD_FAR`]), `None` before [`Self::ensure_depth`].
    pub fn depth_view(&self) -> Option<&wgpu::TextureView> { self.depth.as_ref().map(|d| &d.1) }
    /// SSR BACKDROP: writes the backdrop's depth into the SCENE depth `scene_depth` (cleared to 1 first), converted to the main projection: UE has ONE depth buffer
    /// (reversed-Z, infinite far plane), so the landscape / ocean / terraces are scene geometry for the HZB / SSR, the AO, the fog passes, the TAA reprojection, the
    /// light-shaft and sun masks and the depth test of everything drawn after them. Beyond the main pass's (finite) far plane the depth is clamped just below 1
    /// (still geometry, not sky); see backdrop_merge.wgsl.
    pub fn encode_depth_merge(&self, enc: &mut wgpu::CommandEncoder, scene_depth: &wgpu::TextureView) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("backdrop depth merge"), color_attachments: &[],
            // PERF (2026-10-06): the scene depth already holds the pre-pass (it runs first now): loaded, and the merge pipeline keeps the nearer depth (Less)
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: scene_depth, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store }), stencil_ops: None }),
            timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
        });
        let Some((_, _, bg)) = &self.depth else { return };
        pass.set_pipeline(&self.merge);
        pass.set_bind_group(0, bg, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Loads a PNG as RGBA8.
pub fn load_png(p: &Path) -> Result<RgbaImage, String> { Ok(std::fs::read(p).map_err(image::ImageError::IoError).and_then(|b| image::load_from_memory(&b)).map_err(|e| format!("{}: {e}", p.display()))?.to_rgba8()) }

#[cfg(test)]
mod tests {
    use super::*;
    /// SSR BACKDROP: the depth merge shader converts with the pass's own near / far
    #[test]
    fn depth_merge_uses_the_backdrop_planes() {
        let s = fn_live::wgsl!("../shaders/backdrop_merge.wgsl");
        assert!(s.contains(&format!("const BD_NEAR: f32 = {:.1};", BD_NEAR)) && s.contains(&format!("const BD_FAR: f32 = {:.1}e6;", BD_FAR / 1e6)), "backdrop_merge.wgsl planes");
        assert!(s.contains("0.99999994") && s.contains("discard"));
    }
    #[test]
    fn lod_axes_always_end_on_the_last_sample() {
        for s in LOD_STRIDES { let a = lod_axis(128, s); assert_eq!(a[0], 0); assert_eq!(*a.last().unwrap(), 127); assert!(a.windows(2).all(|w| w[1] > w[0])); }
        assert_eq!(lod_axis(128, 1).len(), 128);
    }
    #[test]
    fn game_mapping_matches_the_props() {
        // props: anchor + ((x - cx) * 0.01, 0, (y - cy) * 0.01); heights re-based
        let g = to_game([51152. + 100., 71640. - 200., 1912. + 300.], [51152., 71640., 1912.], Vec3::new(0., 0., -10.), 1912.);
        assert!((g - Vec3::new(1., 3., -12.)).length() < 1e-4);
    }
    #[test]
    fn terrain_mesh_covers_the_component_and_has_skirts() {
        let c = TerrainComp { size: 128, origin: [0.; 3], step: [200., 200., 0.39], heights: vec![32768; 128 * 128], layers: vec![] };
        let t = TerrainSet { layer_names: vec![], comps: vec![c] };
        let m = build_terrain_mesh(&t, 0, [0.; 3], Vec3::ZERO, 0., 0);
        assert_eq!(m.lods.len(), 3);
        assert_eq!(m.lods[0].1 as usize, 127 * 127 * 6 + 4 * 127 * 6);
        assert!(m.verts[0].normal[1] > 0.99);
        assert_eq!(m.verts[0].color, [255, 0, 0, 0]);
        assert!((sample_height_cm(&t, 100., 100.).unwrap()).abs() < 1e-3);
    }
    /// TERRAIN MAIN PASS (2026-10-07): real heights everywhere (no PLATE_BLEND_M fade inside / around the old plate) and no plate discard in the backdrop shader
    #[test]
    fn terrain_keeps_its_real_height_inside_the_old_plate() {
        let mut heights = vec![32768u16; 128 * 128];
        heights[5 * 128 + 7] = 32768 + 5000;
        let c = TerrainComp { size: 128, origin: [-10000., -10000., 0.], step: [200., 200., 0.5], heights, layers: vec![] };
        let t = TerrainSet { layer_names: vec![], comps: vec![c] };
        let m = build_terrain_mesh(&t, 0, [0.; 3], Vec3::ZERO, 0., 0);
        let want = to_game([-10000. + 7. * 200., -10000. + 5. * 200., t.comps[0].z_cm(7, 5)], [0.; 3], Vec3::ZERO, 0.);
        assert!(want.x.abs() < 256. && want.z.abs() < 256. && want.y > 20.);
        assert!((Vec3::from(m.verts[5 * 128 + 7].pos) - want).length() < 1e-4, "the vertex keeps its raw height");
        let s = fn_live::wgsl!("../shaders/backdrop.wgsl");
        assert!(!s.contains("abs(in.wp.x) < bd.misc.x"), "backdrop.wgsl still cuts the plate out");
    }
    /// TERRAIN MAIN PASS: the height lattice of the global distance field = the mesh vertices (same mapping), holes where no component is
    #[test]
    fn height_grid_matches_the_mesh_and_marks_holes() {
        let mk = |ox: f32, oy: f32, h: u16| TerrainComp { size: 4, origin: [ox, oy, 100.], step: [200., 200., 0.5], heights: vec![h; 16], layers: vec![] };
        // two components side by side sharing a border column, and one diagonal: the fourth lattice cell is a hole
        let t = TerrainSet { layer_names: vec![], comps: vec![mk(0., 0., 32768), mk(600., 0., 32868), mk(600., 600., 32968)] };
        let (centre, anchor, rz) = ([300., 300., 0.], Vec3::new(1., 0., -2.), 50.);
        let g = height_grid(&t, centre, anchor, rz).unwrap();
        assert_eq!((g.w, g.h), (7, 7));
        assert!((g.cell - 2.).abs() < 1e-6);
        for ci in 0..3 {
            let m = build_terrain_mesh(&t, ci, centre, anchor, rz, 0);
            for v in m.verts.iter().take(16) {
                let (i, j) = (((v.pos[0] - g.origin[0]) / g.cell).round() as usize, ((v.pos[2] - g.origin[1]) / g.cell).round() as usize);
                let y = g.y[j * g.w as usize + i];
                // the shared border row / column holds the value written last; the interior samples their own component's
                if i == 3 || j == 3 { continue; }
                assert!((y - v.pos[1]).abs() < 1e-4, "comp {ci} ({i}, {j}): grid {y} vs mesh {}", v.pos[1]);
            }
        }
        assert_eq!(g.y[6 * 7], HEIGHT_HOLE, "no component at the lattice corner (0, 6)");
    }
    /// TERRAIN MAIN PASS: the DF heightfield's lattice lookup (distance_field_ao.wgsl cs_gdf_composite: DF xy = game (x, z) * 100, `(xy - origin_cm) / cell_cm`)
    /// lands on the height fn_terrain's World (collision) reports at that game point (catches an axis swap / offset / re-base mistake)
    #[test]
    fn height_grid_df_lookup_matches_the_collision_world() {
        let mk = |ox: f32, oy: f32, seed: u32| TerrainComp { size: 8, origin: [ox, oy, -1692.], step: [200.787, 200.787, 50. / 128.],
            heights: (0..64u32).map(|k| (32768 + (k * 37 + seed * 101) % 3000) as u16).collect(), layers: vec![] };
        let set = || TerrainSet { layer_names: vec![], comps: vec![mk(-153000., -153000., 1), mk(-153000. + 7. * 200.787, -153000., 2)] };
        let (centre, anchor, rz) = ([-152500., -152700., 0.], Vec3::new(3., 0., 12.), 768.);
        let g = height_grid(&set(), centre, anchor, rz).unwrap();
        let w = fn_terrain::World { hf: fn_terrain::Heightfield::new(set()).unwrap(), map: fn_terrain::Mapping { centre_cm: centre, anchor, ref_z_cm: rz } };
        let (o_cm, cell_cm) = ([g.origin[0] * 100., g.origin[1] * 100.], g.cell * 100.);
        for (i, j) in [(1usize, 1usize), (5, 6), (9, 2), (13, 6), (7, 3)] {
            let (x, z) = (g.origin[0] + i as f32 * g.cell, g.origin[1] + j as f32 * g.cell);
            let df = [x * 100., z * 100.];
            let (gi, gj) = (((df[0] - o_cm[0]) / cell_cm).round() as usize, ((df[1] - o_cm[1]) / cell_cm).round() as usize);
            assert_eq!((gi, gj), (i, j));
            let want = w.ground_y(x, z).unwrap();
            assert!((g.y[gj * g.w as usize + gi] - want).abs() < 2e-3, "({i}, {j}): grid {} vs collision {want}", g.y[gj * g.w as usize + gi]);
        }
    }
}
