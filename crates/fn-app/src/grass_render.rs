//! GRASS (2026-10-06): the ambient ground cover of `Athena_Terrain` (foliage.rs: the `FoliageInstancedStaticMeshComponent`s of `S_WavyGrass_01`) drawn with its
//! REAL cooked material, `M_FieldGrassMatching`, through the instanced static-mesh vertex factory (`FInstancedStaticMeshVertexFactory`, fn-fx
//! `Vf::InstancedStatic`), instead of the old static-mesh path (the terrain texture on the mesh UVs, opaque, no wind: the dark blocky cards).
//!
//! What the cooked programs do (the 1.8 shader map, decoded with crates/fn-fx/examples/grass_probe.rs):
//! * VS `TBasePassVSFNoLightMapPolicy`: the instance transform (`FInstanceStream16`), the per-instance hide / fade / dithered-LOD values of
//!   `FInstancedStaticMeshVertexFactoryShaderParameters` (cb0[6..11]), and only for a shown instance the wind WPO: T_FieldGrass_01_M at WorldPosition.xy /
//!   HeightVarianceScale (1000) lifts the tips up to StaticWPOHigh (30 cm) and sets the sway angle (x Rotation Amount 0.4 x 2 pi), T_SwirlNoise_N at
//!   WorldPosition.xy / WindWaveScale (1200) panned by View.GameTime x WindSpeed (0.04) gives the rotation axis (x WindPower 0.5), about the pivot the
//!   mesh stores in its vertex colour / UV2-3.
//! * PS `TBasePassPSFNoLightMapPolicy`: the blade normal (T_WavyGrass_01_N at UV0) blended to the vertex normal by saturate(UV1.y ^ VertNrmBlndPwr x
//!   VertNrmBlndX); base colour = `GrassTextureToMatch` (T_Grasslands_D, the terrain's grass) at -WorldPosition.xy / GrassColorMaskSize_2 (800 cm) with mip
//!   bias 5, times lerp(GrassTipTint 1.2, 1, saturate(UV1.y x TipMaskAmountB 1.5)); Specular 0.05, Roughness 1, DefaultLit. Masked: `FDepthOnlyPS` clips
//!   T_WavyGrass_01.g < 0.3333 and dithers by PerInstanceParams.w. Two-sided.
//! * The SetMesh values (UE4.16 InstancedStaticMesh.cpp:1903) and the HISM proxy's per-view state are fn-fx `InstancingParams::hism` / `TemporalLod`
//!   (`lod.TemporalLag` 0.5 s on `ViewFamily.CurrentRealTime`).
//! CPU culling: the 32 m cells of foliage.rs stand in for the HISM cluster tree (HierarchicalInstancedStaticMesh.cpp:1338-1530): a cell is dropped only when
//! it is outside the frustum (side planes; the HISM removes the far plane) or farther than FinalCull = min(ComputeBoundsDrawDistance(foliage.MinimumScreenSize),
//! EndCullDistance x r.ViewDistanceScale) from the view and both temporal LOD origins: never tighter than the tree, so the vertex shader makes the same
//! per-instance cut as in 1.8 (at EndCullDistance x scale x scale, SetMesh applies the scale twice).
//! Component (cooked FoliageInstancedStaticMeshComponent, e.g. Athena_POI_Bridge_001 export 252): no OverrideMaterials (the mesh's material), CastShadow
//! false (no cascades), bAffectDistanceFieldLighting false (no DF object), bReceivesDecals false, Mobility Static (velocity only through the material's WPO,
//! VelocityRendering.cpp HasVelocityOnBasePass, as statics_render.rs).
//! LIMITS (named): LOD 0 only (S_WavyGrass_01 has one LOD; a multi-LOD foliage mesh needs the per-LOD batches of the HISM traversal, logged); no HISM node
//! occlusion queries (they only skip hidden nodes); the instance tilt is the flat one of foliage.rs (yaw only; TERRAIN: the height is the landscape's);
//! VELOCITY (2026-10-07): the WPO wind writes velocity like UE's base pass (SV_Target4 of the cooked PS, `HasVelocityOnBasePass`: bModifiesMeshPosition): fn-fx
//! `fs_velocity` twin for the renderer's velocity pass (fn-render fx.rs `draw_velocity`). GRASS MOTION (2026-10-07): ON by default (FN_FX_VELOCITY=0 = off):
//! the VS's previous position = PrevFrameGameTime wind (View 128.z) through PrevTranslatedWorldToClip / PrevPreViewTranslation (View 83..86 / 105, fn-render
//! taa.rs ViewHistory), and the View buffer carries the TAA jitter (taa.rs apply_view_jitter), so the swaying blades no longer ghost under the TAA.
use crate::foliage::{Grass, GrassSource, CELL_M};
use crate::statics_render::{PS_TYPES, VS_TYPE};
use fn_fx::gpu::{build_material_layout, compute_bounds_draw_distance, ism_instance, HismBatch, InstancingParams, LayoutOpts, MaterialGpu, SharedBuffers, TemporalLod, TextureCache, Vf, ISM_INSTANCE_STRIDE};
use fn_fx::material::Material;
use fn_fx::view::{bytes, ViewParams};
use fn_pak::PakSet;
use fn_render::fx::*;
use glam::{Mat3, Mat4, Vec3};
use std::collections::HashMap;

/// `foliage.MinimumScreenSize`, `foliage.LODDistanceScale`, `foliage.RandomLODRange` (HierarchicalInstancedStaticMesh.cpp:72-85 defaults; the 1.8 cooked
/// config sets none of them).
pub const FOLIAGE_MIN_SCREEN_SIZE: f32 = 0.000005;
pub const FOLIAGE_LOD_DISTANCE_SCALE: f32 = 1.0;
pub const FOLIAGE_RANDOM_LOD_RANGE: f32 = 0.0;
/// The FX pass's per-frame instance buffer (fn-render fx.rs `DYN_CAPACITY`, shared with the mesh particles): the grass keeps itself well inside it.
const INSTANCE_BUDGET_BYTES: usize = 768 * 1024;

struct GrassGpu {
    mat: Material,
    gpu: MaterialGpu,
    mesh: FxMeshId,
    /// (first index, index count) of the LOD-0 sections (all slot 0).
    sections: Vec<(u32, u32)>,
    vs_buf: Option<FxBufId>,
    binds: Vec<(u32, FxBindId)>,
    velocity: bool,
    /// `RenderData->Bounds.SphereRadius` and `ScreenSize[]` (the HISM LOD / cull inputs) and the LOD count.
    sphere_radius: f32,
    screen_size: [f32; 8],
    num_lods: usize,
    /// Top of the mesh bounds (cm) for the cell boxes.
    mesh_top_cm: f32,
}

/// What the last frame did (run report / tests).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GrassStats { pub cells: u32, pub cells_drawn: u32, pub instances: u32, pub draws: u32, pub cpu_cull_cm: f32, pub vs_cut_cm: f32, pub cells_occluded: u32 }
fn grass_occlusion() -> bool { static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new(); *ON.get_or_init(|| fn_live::var("FN_GRASS_OCCLUSION").map_or(true, |v| v != "0")) }

pub struct GrassRender {
    gpu: Option<Result<GrassGpu, String>>,
    temporal: TemporalLod,
    /// The `FInstanceStream16` bytes of each cell's blades (encoded once) and the largest blade scale (cell box padding).
    cell_bytes: HashMap<(i32, i32), Vec<u8>>,
    /// TERRAIN: the `Grass::generation` the cell bytes were encoded at (the blades move onto the landscape once it is installed).
    cell_generation: u32,
    max_scale: Option<f32>,
    warned_budget: bool,
    pub stats: GrassStats,
    pub log: Vec<String>,
}

/// The instance matrix rows of a blade in the FX space (UE cm, Z up): the static-mesh path's `R_y(yaw) * scale` conjugated by the (x, z, y) axis map =
/// `Rz(-yaw) * S(scale)` (statics_render::local_to_world), row i = the image of local axis i (the glam column).
pub fn blade_rows(yaw_game: f32, scale: f32) -> [[f32; 3]; 3] {
    let m = Mat3::from_rotation_z(-yaw_game) * Mat3::from_diagonal(Vec3::splat(scale));
    [m.x_axis.to_array(), m.y_axis.to_array(), m.z_axis.to_array()]
}
/// One blade as the instance stream holds it (origin in FX space cm, PerInstanceRandom).
pub fn blade_instance(b: &crate::foliage::Blade) -> [u8; ISM_INSTANCE_STRIDE as usize] {
    let o = fn_fx::view::to_ue(b.pos);
    ism_instance([o.x as f32, o.y as f32, o.z as f32], b.random, blade_rows(b.yaw, b.scale))
}

/// `FMaterial::MaterialModifiesMeshPosition_RenderThread` from the cooked shader map (statics_render.rs `modifies_position`).
fn modifies_position(set: &PakSet, mat: &Material) -> Option<bool> {
    let (a, x, b) = fn_fx::data::package_files(set, &mat.shader_map_package).ok()?;
    let p = fn_ueio::Package::parse(&a, &x, &b).ok()?;
    fn_fx::uniform::compilation_flags(&x, &p.names).map(|f| f.modifies_mesh_position)
}

fn build(set: &PakSet, fx: &mut FxRenderer, cache: &mut TextureCache, shared: SharedBuffers, src: &GrassSource, log: &mut Vec<String>) -> Result<GrassGpu, String> {
    let mat = Material::load(set, &src.material_ue_path)?;
    let vf = Vf::InstancedStatic;
    if mat.cooked.find(vf.type_name(), VS_TYPE).is_none() { return Err(format!("{}: no {} / {VS_TYPE} program", src.material_ue_path, vf.type_name())); }
    let ps = PS_TYPES.iter().copied().find(|ps| mat.cooked.find(vf.type_name(), ps).is_some()).ok_or_else(|| format!("{}: no {} base-pass pixel program", src.material_ue_path, vf.type_name()))?;
    // the cooked mesh: LOD 0, packed as the static-mesh FX path packs it (position, TangentX, TangentZ, colour, UV pairs)
    let long = fn_fx::data::long_package(&src.mesh_ue_path).to_owned();
    let (a, x, b) = fn_fx::data::package_files(set, &long)?;
    let p = fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))?;
    let m = fn_mesh::StaticMesh::parse(&p).map_err(|e| format!("{long}: {e}"))?;
    let lod = m.lods.first().ok_or_else(|| format!("{long}: no LOD"))?;
    if m.lods.len() > 1 { log.push(format!("LIMIT: {long} has {} LODs, only LOD 0 is drawn (the HISM per-LOD batches are not ported)", m.lods.len())); }
    let num_uv = lod.num_tex_coords;
    let stride = fn_fx::gpu::mesh_stride(vf, num_uv) as usize;
    let n = lod.positions.len();
    let mut vb = vec![0u8; n * stride];
    for i in 0..n {
        let o = i * stride;
        for (k, f) in lod.positions[i].iter().enumerate() { vb[o + 4 * k..o + 4 * k + 4].copy_from_slice(&f.to_le_bytes()); }
        vb[o + 12..o + 16].copy_from_slice(&lod.tangent_x[i]);
        vb[o + 16..o + 20].copy_from_slice(&lod.tangent_z[i]);
        vb[o + 20..o + 24].copy_from_slice(&lod.colors.get(i).copied().unwrap_or([255; 4]));
        for u in 0..num_uv {
            let uv = lod.uvs.get(u).and_then(|s| s.get(i)).copied().unwrap_or([0.; 2]);
            let at = o + 24 + 16 * (u / 2) + 8 * (u % 2);
            vb[at..at + 4].copy_from_slice(&uv[0].to_le_bytes()); vb[at + 4..at + 8].copy_from_slice(&uv[1].to_le_bytes());
        }
    }
    let mesh = fx.create_mesh(&FxMeshDesc { label: format!("{} LOD0 (grass)", m.name), vertices: vb, indices: lod.indices.clone() });
    let mut sections = vec![];
    for s in &lod.sections {
        if s.material_index.max(0) != 0 { log.push(format!("{long}: section with material slot {} skipped (the component draws slot 0's material)", s.material_index)); continue; }
        sections.push((s.first_index, s.num_triangles * 3));
    }
    let gpu = build_material_layout(set, fx, cache, &mat, vf, VS_TYPE, ps, num_uv, true, shared, &[], LayoutOpts::default())?;
    if gpu.vs_globals().map_or(true, |g| g.params.instanced_vf().is_none()) { return Err(format!("{}: the instanced VS has no FInstancedStaticMeshVertexFactory parameters", src.material_ue_path)); }
    // the component's Primitive buffer: identity LocalToWorld (the instance origins are FX-space cm), DecalReceiverMask 0 (bReceivesDecals false), no distance field
    let prim = fx.create_buffer("grass primitive", 32 * 16);
    let local = fn_render::primitive_ub::BoxSphereBounds { origin: m.bounds.origin, extent: m.bounds.extent, radius: m.bounds.radius };
    let flags = fn_render::primitive_ub::PrimitiveFlags { receives_decals: false, has_distance_field: false, ..Default::default() };
    fx.write_buffer(prim, &bytes(&fn_render::primitive_ub::mesh_primitive(&Mat4::IDENTITY, &local, [0.; 3], &flags)));
    let vs_buf = gpu.vs_globals().map(|g| fx.create_buffer("grass VS globals", g.float4s * 16));
    let binds = gpu.bind_variant_globals(fx, prim, None, vs_buf);
    let velocity = modifies_position(set, &mat).unwrap_or(false);
    let line = format!("grass: {} ({} tris, {} UV sets, {} LODs) with {} ({} / {ps}, {} {} two-sided {}), bModifiesMeshPosition {velocity}, cull {} / {} cm, cast shadow {}",
        m.name, lod.indices.len() / 3, num_uv, m.lods.len(), mat.path.rsplit('.').next().unwrap_or(""), vf.type_name(), mat.flags.blend, mat.flags.shading, mat.flags.two_sided, src.start_cull_cm, src.end_cull_cm, src.cast_shadow);
    eprintln!("[grass] {line}");
    log.push(line);
    Ok(GrassGpu { mat, gpu, mesh, sections, vs_buf, binds, velocity, sphere_radius: m.bounds.radius, screen_size: m.screen_size, num_lods: m.lods.len(), mesh_top_cm: m.bounds.origin[2] + m.bounds.extent[2] })
}

impl GrassRender {
    pub fn new() -> GrassRender { GrassRender { gpu: None, temporal: TemporalLod::default(), cell_bytes: HashMap::new(), cell_generation: 0, max_scale: None, warned_budget: false, stats: GrassStats::default(), log: vec![] } }
    /// The real-shader path is built and draws the grass (the old static-mesh path then leaves the blades out).
    pub fn active(&self) -> bool { matches!(self.gpu, Some(Ok(_))) }

    /// One frame: the instanced draw of the cells that pass the HISM culling, with this frame's SetMesh values. Returns true when the path became active this frame.
    pub fn prebuild(&mut self, set: &PakSet, cache: &mut TextureCache, fx: &mut FxRenderer, shared: SharedBuffers, grass: &Grass) -> bool {
        let Some(src) = grass.source.clone() else { return false };
        if self.gpu.is_some() { return false; }
        let t0 = std::time::Instant::now();
        let r = build(set, fx, cache, shared, &src, &mut self.log);
        let ok = r.is_ok();
        match &r {
            Ok(_) => self.log.push(format!("grass: real-shader path built in {:.0} ms", t0.elapsed().as_secs_f32() * 1000.)),
            Err(e) => { let m = format!("grass: NOT ON THE REAL-SHADER PATH ({e}): the old static-mesh grass stays"); eprintln!("[grass] {m}"); self.log.push(m); }
        }
        self.gpu = Some(r);
        ok
    }

    pub fn frame(&mut self, set: &PakSet, cache: &mut TextureCache, fx: &mut FxRenderer, shared: SharedBuffers, grass: &Grass, view: &ViewParams, view_distance_scale: f32, out: &mut FxFrame) -> bool {
        let Some(src) = grass.source.clone() else { return false };
        let mut became_active = false;
        if self.gpu.is_none() {
            let t0 = std::time::Instant::now();
            let r = build(set, fx, cache, shared, &src, &mut self.log);
            match &r {
                Ok(_) => { self.log.push(format!("grass: real-shader path built in {:.0} ms", t0.elapsed().as_secs_f32() * 1000.)); became_active = true; }
                Err(e) => { let m = format!("grass: NOT ON THE REAL-SHADER PATH ({e}): the old static-mesh grass stays"); eprintln!("[grass] {m}"); self.log.push(m); }
            }
            self.gpu = Some(r);
        }
        let Some(Ok(g)) = self.gpu.as_ref() else { return became_active };
        if grass.generation() != self.cell_generation { self.cell_generation = grass.generation(); self.cell_bytes.clear(); } // TERRAIN
        let max_scale = *self.max_scale.get_or_insert_with(|| grass.cells().values().flatten().map(|b| b.scale).fold(1.0f32, f32::max));
        // the view (FX space, UE cm) and the HISM proxy's per-view state, updated once per frame like FSceneViewState after visibility
        let e = fn_fx::view::to_ue(view.eye);
        let eye = [e.x as f32, e.y as f32, e.z as f32];
        self.temporal.update(eye, view.real_time);
        let alpha = self.temporal.transition(view.real_time);
        let origins = [self.temporal.origin(0, eye), self.temporal.origin(1, eye)];
        let proj_scale = (1.0 / view.tan_half.0, 1.0 / view.tan_half.1);
        let inst = InstancingParams::hism(&HismBatch {
            start_cull_cm: src.start_cull_cm, end_cull_cm: src.end_cull_cm, view_distance_scale, lod: 0, first_lod: 0, screen_size: g.screen_size, num_lods: g.num_lods,
            sphere_radius: g.sphere_radius, proj_scale, min_screen_size: FOLIAGE_MIN_SCREEN_SIZE, lod_scale: FOLIAGE_LOD_DISTANCE_SCALE, lod_random: FOLIAGE_RANDOM_LOD_RANGE,
            temporal_origin: origins, temporal_alpha: alpha, dithered: true,
        });
        // HISM CPU cut (HierarchicalInstancedStaticMesh.cpp:1471-1480): FinalCull without the vertex shader's second scale
        let mut cpu_cull = compute_bounds_draw_distance(FOLIAGE_MIN_SCREEN_SIZE, g.sphere_radius, proj_scale) * FOLIAGE_LOD_DISTANCE_SCALE;
        if src.end_cull_cm > 0.0 { cpu_cull = cpu_cull.min(src.end_cull_cm * view_distance_scale); }
        let pad = g.sphere_radius * max_scale;
        let fwd = fn_fx::view::dir_to_ue(view.forward).as_vec3().normalize();
        let right = fwd.cross(fn_fx::view::dir_to_ue(view.up).as_vec3()).normalize();
        let up = right.cross(fwd).normalize();
        let (tx, ty) = view.tan_half;
        let (sx, sy) = ((1. + tx * tx).sqrt(), (1. + ty * ty).sqrt());
        let eye_v = Vec3::from(eye);
        let cell_cm = CELL_M * 100.0;
        let top = g.mesh_top_cm * max_scale;
        let mut inst_bytes: Vec<u8> = Vec::new();
        let mut st = GrassStats { cells: grass.cells().len() as u32, cpu_cull_cm: cpu_cull, vs_cut_cm: inst.view_z_compare_zero[2], ..Default::default() };
        let occ = if grass_occlusion() { fn_render::occlusion::latest() } else { None };
        let mut keys: Vec<&(i32, i32)> = grass.cells().keys().collect();
        keys.sort_unstable(); // a stable submission order (the HashMap order is not)
        for key in keys {
            let blades = &grass.cells()[key];
            // the cell box in FX space: game x -> UE x, game z -> UE y (foliage.rs cell_of), UE z from the lowest blade base to the tallest blade (TERRAIN: the
            // blades stand on the landscape)
            let (x0, y0) = (key.0 as f32 * cell_cm, key.1 as f32 * cell_cm);
            let (b0, b1) = grass.cell_y(*key);
            let (lo, hi) = (Vec3::new(x0, y0, b0 * 100.0) - Vec3::splat(pad), Vec3::new(x0 + cell_cm, y0 + cell_cm, b1 * 100.0 + top) + Vec3::splat(pad));
            let dist = |o: [f32; 3]| { let o = Vec3::from(o); (lo - o).max(o - hi).max(Vec3::ZERO).length() };
            if dist(eye).min(dist(origins[0])).min(dist(origins[1])) > cpu_cull { continue; }
            let c = (lo + hi) * 0.5;
            let r = (hi - lo).length() * 0.5;
            let d = c - eye_v;
            let z = d.dot(fwd);
            if !(z + r > 0.0 && d.dot(right).abs() <= z * tx + r * sx && d.dot(up).abs() <= z * ty + r * sy) { continue; }
            if occ.as_ref().is_some_and(|o| o.sphere_occluded(Vec3::new(c.x, c.z, c.y) / 100., r / 100., view.eye)) { st.cells_occluded += 1; continue; }
            let cb = self.cell_bytes.entry(*key).or_insert_with(|| blades.iter().flat_map(blade_instance).collect());
            if inst_bytes.len() + cb.len() > INSTANCE_BUDGET_BYTES {
                if !self.warned_budget { self.warned_budget = true; let m = format!("grass: more than {} KB of instances in view: the farthest cells of this frame are left out (FX instance buffer)", INSTANCE_BUDGET_BYTES / 1024); eprintln!("[grass] {m}"); self.log.push(m); }
                break;
            }
            inst_bytes.extend_from_slice(cb);
            st.cells_drawn += 1;
            st.instances += blades.len() as u32;
        }
        // the material buffer (uniform expressions at this frame's time), the per-frame globals and this batch element's VS globals (SetMesh + velocity)
        let t = view.material_time();
        let packed = fn_fx::uniform::evaluate(&g.mat.uniform, &g.mat.overrides, t.world);
        let mut cb = packed.cb; cb.resize(cb.len().max(g.gpu.material_registers).max(1), [0.0; 4]);
        out.buffer_writes.push((shared.view, bytes(&view.view_buffer())));
        out.buffer_writes.push((shared.light, bytes(&fn_render::sky_light::fx_light_block())));
        out.buffer_writes.push((g.gpu.material_buf, bytes(&cb)));
        out.buffer_writes.extend(g.gpu.per_frame_writes(&g.mat, t));
        // a Static component: velocity on the base pass only through the material's WPO, PreviousLocalToWorld = LocalToWorld (statics_render.rs rule)
        if let (Some(b), Some(by)) = (g.vs_buf, g.gpu.vs_globals_bytes_instanced(&g.mat, t, if g.velocity { Some(Mat4::IDENTITY) } else { None }, &inst)) { out.buffer_writes.push((b, by)); }
        if st.instances > 0 {
            for &(first, count) in &g.sections {
                out.draws.push(FxDraw { pipe: g.gpu.pipe, binds: g.binds.clone(), geom: FxGeom::MeshRange { mesh: g.mesh, first_index: first, index_count: count }, instances: Some(FxInstances { bytes: inst_bytes.clone(), count: st.instances }), sort: view.sort(eye) });
                st.draws += 1;
            }
        }
        self.stats = st;
        became_active
    }

    pub fn report(&self) -> String {
        if self.log.is_empty() { return String::new(); }
        let s = &self.stats;
        format!("grass (real cooked shaders): {} of {} cells drawn ({} occluded), {} instances, {} draws, CPU cull {:.0} cm, VS cut {:.0} cm\n  {}\n", s.cells_drawn, s.cells, s.cells_occluded, s.instances, s.draws, s.cpu_cull_cm, s.vs_cut_cm, self.log.join("\n  "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The blade's instance rows place a local vertex exactly where the static-mesh path's transform puts it (statics_render::local_to_world, yaw + scale).
    #[test] fn blade_rows_match_the_static_transform() {
        for (yaw, scale) in [(0.0f32, 1.0f32), (0.7, 1.3), (-2.4, 0.8)] {
            let rows = blade_rows(yaw, scale);
            let l2w = crate::statics_render::local_to_world(Vec3::new(3., 0., -7.), yaw, scale, false);
            let local = Vec3::new(12., -40., 30.);
            let via_rows = Vec3::from(rows[0]) * local.x + Vec3::from(rows[1]) * local.y + Vec3::from(rows[2]) * local.z + l2w.w_axis.truncate();
            assert!((via_rows - l2w.transform_point3(local)).length() < 1e-3, "{yaw} {scale}");
        }
    }
    /// The instance origin is the blade's FX-space position (UE cm) and w the PerInstanceRandom.
    #[test] fn blade_instance_origin() {
        let b = crate::foliage::Blade { pos: Vec3::new(1.5, 0., -2.25), yaw: 0.3, scale: 1.1, random: 0.625 };
        let i = blade_instance(&b);
        let f = |o: usize| f32::from_le_bytes(i[o..o + 4].try_into().unwrap());
        assert_eq!((f(0), f(4), f(8), f(12)), (150.0, -225.0, 0.0, 0.625));
    }
}
