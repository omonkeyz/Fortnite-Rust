//! BACKDROP REAL (2026-10-07): the far backdrop of `Athena_Terrain` (the placed `FortStaticMeshActor`s of the persistent level: two `S_StoneLarge_01_Skybox`
//! rocks and four `S_Terrace_01` island terraces, 3-11 km out; `fn-export props backdrop-actors` -> `backdrop.json`) drawn with their REAL cooked material
//! shaders (`M_Skybox_Temperate_Terraces_01_moretiles` / `_Inst`, root `M_Skybox_World_01`: WorldAlignedBlend, Painted Height and its own `SkyboxFog*`
//! distance haze) instead of the reconstructed `Kind::Skybox` shading of fn-render backdrop.rs.
//!
//! Inventory (fn-inspect over the persistent level + all 35 always-loaded sublevels, 2026-10-07): these six are the only far meshes of the map; the
//! `Athena_Terrain_LS_0N` sublevels hold only island landscape / cliffs / props and their HLOD `LODActor` proxies (all on the island), the plain
//! `StaticMeshActor`s of LS_02/03/05 are sidewalks / ground pieces on the island, the persistent `InstancedFoliageActor` has no instances.
//!
//! Machinery = statics_render.rs (same cooked `UStaticMesh` LODs, `FLocalVertexFactory` + `TBasePassPSFNoLightMapPolicy`, scene-lit assembly), but:
//! * own `View` buffer with the BACKDROP projection (near [`fn_render::backdrop::BD_NEAR`], far [`fn_render::backdrop::BD_FAR`]), so 11 km is inside the frustum;
//! * the pipelines are marked as backdrop pipelines (`FxRenderer::mark_backdrop_pipe`): drawn in the renderer's backdrop pass (own depth target, depth twin then
//!   Equal colour), merged into the scene depth like the landscape / ocean;
//! * no cascades / distance field (beyond the shadow distance; the meshes have `CastShadow` but no caster reaches them at 3+ km).
//! World space = the FX space of every other draw (`fn_fx::view::to_ue(game)`): the real UE position shifted by the props' layout mapping
//! (`(anchor * 100 - centre)` in x / y, `-ref_z` in z). NOT exact (named): the material's absolute-world terms (WorldAlignedBlend tiling phase, Painted Height)
//! see that shifted position.
//! `FN_BACKDROP_REAL=0` turns this off (the reconstructed `Kind::Skybox` draw comes back).
//! MPC_Skybox_World (2026-10-07): the program's skybox fog / lit terms come from that collection, which `TODM_A` fills from its `SkyboxFog*` properties
//! ([`skybox_world_instance`]); with the cooked defaults (Distance 0) every terrace was one flat fog-coloured silhouette, which is why this was off.
//! LIMITS: a failed build (no install / paks) leaves no far meshes, since the loader already dropped them from the reconstructed draw (backdrop_load);
//! the instance values are the construction script's, written once (the ubergraph rewrite at 0xa64 writes the same properties, which nothing changes in Athena).
use fn_fx::gpu::{build_material_layout, LayoutOpts, MaterialGpu, SharedBuffers, TextureCache, Vf};
use fn_fx::material::Material;
use fn_fx::view::{bytes, sprite_vf_buffer, ViewParams, VIEW_FLOAT4S};
use fn_pak::PakSet;
use fn_render::fx::*;
use glam::{Mat4, Quat, Vec3};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub fn enabled() -> bool { fn_live::var("FN_BACKDROP_REAL").map_or(true, |v| v != "0") } // ON by default (2026-10-07, once MPC_Skybox_World got the TOD manager values); FN_BACKDROP_REAL=0 = the old reconstructed draw

/// `MPC_Skybox_World`: the collection the `M_Skybox_World_01` programs read as `MaterialCollection` (16 vectors, 4 per day phase: `<P>Color`, `<P>LitColor`,
/// `<p>_R=Distance_G=DiffuseAmt_B=EmissiveAmt`, `<p>_R=LitLength_G=LitRot_B=LitOffset`), blended in the pixel program by `FortniteMaterialParameters.
/// FortniteActiveTimeOfDay` (mpc_hook.rs) and used as `1 / max(Distance, 1e-5)` etc. Most of its cooked `DefaultValue`s are absent (= 0, so Distance 0 ->
/// the whole mesh is the flat fog colour); the 1.8 game never draws it with those: `TODM_A` writes all 16 in its `UserConstructionScript` (offsets 0x50..0x590)
/// and again in its ubergraph (0xa64..0xfa4) from its `SkyboxFog*` properties.
pub const MPC_SKYBOX_WORLD: &str = "/Game/Environments/World/Backgrounds/Universal/MasterMaterials/MPC_Skybox_World.MPC_Skybox_World";
/// The class `Athena_Terrain`'s `WorldSettings.WorldTimeOfDayManager` spawns (fn-matshader sky.rs) and its parent: a Blueprint CDO serializes only what differs
/// from its parent's CDO, so a `SkyboxFog*` value is the child's when present, else `TODM_A`'s, else the property default 0.
const TODM_CDOS: [(&str, &str); 2] = [("/Game/TimeOfDay/TODM/TODM_A_Child_Athena", "Default__TODM_A_Child_Athena_C"), ("/Game/TimeOfDay/TODM/TODM_A", "Default__TODM_A_C")];

/// The `MPC_Skybox_World` instance values `TODM_A`'s construction script writes (fn-inspect disasm TODM_A `UserConstructionScript`), per phase P / p
/// (Morning / Morn, Day / Day, Evening / Eve, Night / Night):
/// `SetVectorParameterValue(MPC_Skybox_World, <P>Color, SkyboxFog<P>Color)`, `<P>LitColor` = `SkyboxFog<P>LitColor`,
/// `<p>_R=Distance_G=DiffuseAmt_B=EmissiveAmt` = `MakeColor(SkyboxFog<P>Distance, SkyboxFog<P>DiffuseAmount, SkyboxFog<P>EmissiveAmount, 1)`,
/// `<p>_R=LitLength_G=LitRot_B=LitOffset` = `MakeColor(SkyboxFog<P>LitLength, 4000000 + FClamp(SkyboxFog<P>LitRotation, -4e6, 4e6), 4000000 + FClamp(SkyboxFog<P>LitOffset, -4e6, 4e6), 1)`
/// (float adds in f32, as the Kismet VM does).
fn skybox_world_instance(set: &PakSet, c: &fn_fx::mpc::Collection) -> Result<fn_fx::mpc::Instance, String> {
    let mut cdos = vec![];
    for (long, name) in TODM_CDOS {
        let pkg = fn_fx::data::PackageJson::load(set, long)?;
        cdos.push(pkg.by_name(name).ok_or_else(|| format!("{long}: no {name}"))?.props.clone());
    }
    let prop = |k: &str| cdos.iter().find_map(|p| p.get(k).cloned());
    let num = |k: &str| prop(k).and_then(|v| v.as_f64()).map_or(0., |x| x as f32);
    let color = |k: &str| prop(k).and_then(|v| v.as_array().filter(|a| a.len() == 4).map(|a| [0, 1, 2, 3].map(|i| a[i].as_f64().unwrap_or(0.) as f32))).unwrap_or([0.; 4]);
    let mut inst = fn_fx::mpc::Instance::default();
    for (p, s) in [("Morning", "Morn"), ("Day", "Day"), ("Evening", "Eve"), ("Night", "Night")] {
        let f = |k: &str| format!("SkyboxFog{p}{k}");
        let ok = [
            inst.set_vector(c, &format!("{p}Color"), color(&f("Color"))),
            inst.set_vector(c, &format!("{p}LitColor"), color(&f("LitColor"))),
            inst.set_vector(c, &format!("{s}_R=Distance_G=DiffuseAmt_B=EmissiveAmt"), [num(&f("Distance")), num(&f("DiffuseAmount")), num(&f("EmissiveAmount")), 1.]),
            inst.set_vector(c, &format!("{s}_R=LitLength_G=LitRot_B=LitOffset"), [num(&f("LitLength")), 4_000_000f32 + num(&f("LitRotation")).clamp(-4e6, 4e6), 4_000_000f32 + num(&f("LitOffset")).clamp(-4e6, 4e6), 1.]),
        ];
        if ok.contains(&false) { return Err(format!("{MPC_SKYBOX_WORLD}: missing a {p} parameter")); }
    }
    Ok(inst)
}

struct Placement { actor: String, mesh: String, material: String, t: [f32; 3], q: [f32; 4], s: [f32; 3] }
struct Lod { mesh: FxMeshId, num_uv: usize, sections: Vec<(u32, u32)>, tris: u32 }
struct MeshGpu { lods: Vec<Lod>, screen_size: [f32; 8], bounds: fn_render::primitive_ub::BoxSphereBounds }
struct Entry { mat: Arc<Material>, gpu: MaterialGpu }
struct Built { meshes: Vec<MeshGpu>, entries: Vec<Entry>, draws: Vec<(usize, usize, usize, Option<FxBufId>, Vec<(u32, FxBindId)>, Mat4)>, /** MPC_Skybox_World buffer + the TODM values (skybox_world_instance) */ skybox_mpc: Option<(FxBufId, Vec<[f32; 4]>)> }

pub struct BackdropReal {
    set: Arc<PakSet>,
    placements: Vec<Placement>,
    shared: Option<SharedBuffers>,
    cache: TextureCache,
    built: Option<Result<Built, String>>,
    pub stats: (u32, u64),
}

fn f(v: &serde_json::Value, d: f32) -> f32 { v.as_f64().map_or(d, |x| x as f32) }
fn a3(v: &serde_json::Value, d: f32) -> [f32; 3] { [f(&v[0], d), f(&v[1], d), f(&v[2], d)] }

impl BackdropReal {
    fn new(install: &std::path::Path, dir: &std::path::Path) -> Result<BackdropReal, String> {
        let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("backdrop.json")).map_err(|e| format!("backdrop.json: {e}"))?).map_err(|e| e.to_string())?;
        let mut placements = vec![];
        for i in doc["instances"].as_array().ok_or("backdrop.json: instances")? {
            let mesh = i["mesh_path"].as_str().unwrap_or("").to_owned();
            if mesh.contains("Ocean") || mesh.is_empty() { continue; } // the ocean planes stay on backdrop.rs
            let Some(mat) = i["material"]["path"].as_str() else { continue };
            let q = &i["rotation_quat"];
            placements.push(Placement { actor: i["actor"].as_str().unwrap_or("").to_owned(), mesh, material: mat.to_owned(), t: a3(&i["translation_cm"], 0.), q: [f(&q[0], 0.), f(&q[1], 0.), f(&q[2], 0.), f(&q[3], 1.)], s: a3(&i["scale"], 1.) });
        }
        let set = Arc::new(PakSet::open_install(install).map_err(|e| e.to_string())?);
        Ok(BackdropReal { set, placements, shared: None, cache: TextureCache::default(), built: None, stats: (0, 0) })
    }

    fn build(&mut self, fx: &mut FxRenderer) -> Result<Built, String> {
        let t0 = std::time::Instant::now();
        let view = fx.create_buffer("backdrop real view", VIEW_FLOAT4S * 16);
        let light = fx.create_buffer("backdrop real light", fn_render::sky_light::FX_LIGHT_BYTES);
        let sprite_vf = fx.create_buffer("backdrop real sprite vf", 10 * 16);
        fx.write_buffer(sprite_vf, &bytes(&sprite_vf_buffer()));
        let prim0 = fx.create_buffer("backdrop real primitive (shared)", 32 * 16);
        let fade = fx.create_buffer("backdrop real PrimitiveFade", 16);
        fx.write_buffer(fade, &bytes(&fn_matshader::frame::fade_buffer()));
        let shared = SharedBuffers { view, primitive: prim0, sprite_vf, light };
        self.shared = Some(shared);
        let mut meshes: Vec<MeshGpu> = vec![];
        let mut mesh_of: HashMap<String, usize> = HashMap::new();
        let mut entries: Vec<Entry> = vec![];
        let mut entry_of: HashMap<(String, usize), usize> = HashMap::new();
        let mut draws = vec![];
        let map = fn_terrain::world().map(|w| (w.map.centre_cm, w.map.anchor, w.map.ref_z_cm)).ok_or("no landscape mapping installed (backdrop_load)")?;
        for p in &self.placements {
            let mi = match mesh_of.get(&p.mesh) { Some(&i) => i, None => {
                let long = fn_fx::data::long_package(&p.mesh).to_owned();
                let (a, x, b) = fn_fx::data::package_files(&self.set, &long)?;
                let pk = fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))?;
                let m = fn_mesh::StaticMesh::parse(&pk).map_err(|e| format!("{long}: {e}"))?;
                let mut lods = vec![];
                for (li, lod) in m.lods.iter().enumerate() {
                    let num_uv = lod.num_tex_coords;
                    let stride = fn_fx::gpu::mesh_stride(Vf::Local, num_uv) as usize;
                    let n = lod.positions.len();
                    let mut vb = vec![0u8; n * stride];
                    for i in 0..n { // statics_render.rs kind(): position, TangentX, TangentZ, colour, UV pairs
                        let o = i * stride;
                        for (k, v) in lod.positions[i].iter().enumerate() { vb[o + 4 * k..o + 4 * k + 4].copy_from_slice(&v.to_le_bytes()); }
                        vb[o + 12..o + 16].copy_from_slice(&lod.tangent_x[i]);
                        vb[o + 16..o + 20].copy_from_slice(&lod.tangent_z[i]);
                        vb[o + 20..o + 24].copy_from_slice(&lod.colors.get(i).copied().unwrap_or([255; 4]));
                        for u in 0..num_uv {
                            let uv = lod.uvs.get(u).and_then(|s| s.get(i)).copied().unwrap_or([0.; 2]);
                            let at = o + 24 + 16 * (u / 2) + 8 * (u % 2);
                            vb[at..at + 4].copy_from_slice(&uv[0].to_le_bytes()); vb[at + 4..at + 8].copy_from_slice(&uv[1].to_le_bytes());
                        }
                    }
                    let mesh = fx.create_mesh(&FxMeshDesc { label: format!("{} LOD{li}", m.name), vertices: vb, indices: lod.indices.clone() });
                    let sections: Vec<(u32, u32)> = lod.sections.iter().map(|s| (s.first_index, s.num_triangles * 3)).collect();
                    let tris = sections.iter().map(|s| s.1 / 3).sum();
                    lods.push(Lod { mesh, num_uv, sections, tris });
                }
                if lods.is_empty() { return Err(format!("{long}: no LOD")); }
                let lod_line: Vec<String> = lods.iter().zip(m.screen_size.iter()).map(|(l, s)| format!("{} tris @ {s:.3}", l.tris)).collect();
                eprintln!("[backdrop real] mesh {}: {} LODs ({}), {} UV channels", m.name, lods.len(), lod_line.join(", "), lods[0].num_uv);
                meshes.push(MeshGpu { lods, screen_size: m.screen_size, bounds: fn_render::primitive_ub::BoxSphereBounds { origin: m.bounds.origin, extent: m.bounds.extent, radius: m.bounds.radius } });
                mesh_of.insert(p.mesh.clone(), meshes.len() - 1);
                meshes.len() - 1
            } };
            // every LOD's UV count gets its material entry (the vertex layout depends on it)
            for li in 0..meshes[mi].lods.len() {
                let num_uv = meshes[mi].lods[li].num_uv;
                if entry_of.contains_key(&(p.material.clone(), num_uv)) { continue; }
                let mat = Arc::new(Material::load(&self.set, &p.material)?);
                let mut built = None;
                let mut last = String::new();
                for ps in crate::statics_render::PS_TYPES {
                    if mat.cooked.find(Vf::Local.type_name(), ps).is_none() { last = format!("{}: no {} / {ps}", p.material, Vf::Local.type_name()); continue; }
                    fx.set_building_backdrop(true); // the backdrop variant: no pre-pass split, drawn by the backdrop pass
                    let gpu = build_material_layout(&self.set, fx, &mut self.cache, &mat, Vf::Local, crate::statics_render::VS_TYPE, ps, num_uv, false, shared, &[("PrimitiveFade", fade)], LayoutOpts::default());
                    fx.set_building_backdrop(false);
                    let gpu = gpu?;
                    fx.mark_backdrop_pipe(gpu.pipe);
                    eprintln!("[backdrop real] material {} (shader map {}): {} / {ps}, blend {} shading {} two-sided {}, {} vectors / {} scalars / {} textures", p.material.rsplit('.').next().unwrap_or(""), mat.shader_map_package.rsplit('/').next().unwrap_or(""), crate::statics_render::VS_TYPE, mat.flags.blend, mat.flags.shading, mat.flags.two_sided, mat.uniform.vectors.len(), mat.uniform.scalars.len(), mat.uniform.textures2d.len());
                    built = Some(gpu);
                    break;
                }
                let gpu = built.ok_or(last)?;
                entries.push(Entry { mat, gpu });
                entry_of.insert((p.material.clone(), num_uv), entries.len() - 1);
            }
            // UE world -> FX space (the props' mapping, see the module doc)
            let off = Vec3::new(map.1.x * 100. - map.0[0], map.1.z * 100. - map.0[1], -map.2);
            let l2w = Mat4::from_scale_rotation_translation(Vec3::from(p.s), Quat::from_xyzw(p.q[0], p.q[1], p.q[2], p.q[3]), Vec3::from(p.t) + off);
            let prim = fx.create_buffer("backdrop real primitive", 32 * 16);
            fx.write_buffer(prim, &bytes(&fn_render::primitive_ub::mesh_primitive(&l2w, &meshes[mi].bounds, l2w.w_axis.truncate().to_array(), &Default::default())));
            for li in 0..meshes[mi].lods.len() {
                let e = entry_of[&(p.material.clone(), meshes[mi].lods[li].num_uv)];
                let vs = entries[e].gpu.vs_globals().map(|g| fx.create_buffer("backdrop real VS globals", g.float4s * 16));
                let binds = entries[e].gpu.bind_variant_globals(fx, prim, None, vs);
                draws.push((mi, li, e, vs, binds, l2w));
            }
            eprintln!("[backdrop real] {} -> {} at UE {:?} scale {:?}", p.actor, p.mesh.rsplit('.').next().unwrap_or(""), p.t, p.s);
        }
        eprintln!("[backdrop real] {} placements, {} meshes, {} materials built in {:.0} ms", self.placements.len(), meshes.len(), entries.len(), t0.elapsed().as_secs_f32() * 1000.);
        // MPC_Skybox_World: the TOD manager's construction-script values over the shared collection buffer (fn_fx::gpu created it with the cooked defaults)
        let skybox_mpc = match (fx.collection_buffer(MPC_SKYBOX_WORLD), fn_fx::mpc::loaded(MPC_SKYBOX_WORLD)) {
            (Some(buf), Some(c)) => match skybox_world_instance(&self.set, &c) {
                Ok(inst) => { let d = c.data(&inst); eprintln!("[backdrop real] MPC_Skybox_World from TODM_A_Child_Athena / TODM_A (construction script): Day {:?} / {:?}", d[6], d[7]); Some((buf, d)) }
                Err(e) => { eprintln!("[backdrop real] MPC_Skybox_World: {e}: cooked defaults (Distance 0 -> flat fog colour)"); None }
            },
            _ => None,
        };
        Ok(Built { meshes, entries, draws, skybox_mpc })
    }

    fn frame(&mut self, fx: &mut FxRenderer, view: &ViewParams, out: &mut FxFrame) {
        if self.built.is_none() { let r = self.build(fx); if let Err(e) = &r { eprintln!("[backdrop real] NOT BUILT ({e}): the far backdrop is not drawn on the real-shader path"); } self.built = Some(r); }
        let (Some(Ok(b)), Some(shared)) = (self.built.as_ref(), self.shared) else { return };
        // the backdrop pass's projection (backdrop.rs BD_NEAR / BD_FAR)
        let bv = ViewParams { near_m: fn_render::backdrop::BD_NEAR, far_m: fn_render::backdrop::BD_FAR, ..*view };
        out.buffer_writes.push((shared.view, bytes(&bv.view_buffer())));
        out.buffer_writes.push((shared.light, bytes(&fn_render::sky_light::fx_light_block())));
        if let Some((buf, d)) = &b.skybox_mpc { out.buffer_writes.push((*buf, bytes(d))); } // MPC_Skybox_World (unchanged frames are skipped by FxRenderer::prepare)
        let t = bv.material_time();
        let eye = fn_fx::view::to_ue(bv.eye).as_vec3();
        let mut written = std::collections::HashSet::new();
        let mut st = (0u32, 0u64);
        // PERF (2026-10-08, FN_BACKDROP_FRUSTUM=0 = off, shared with fn_render::backdrop): a far mesh whose bounding sphere lies wholly outside one of the view's 4
        // side planes covers no pixel and is not submitted (it was drawn whole and clipped, terraces behind the camera included). The sphere grows by 0.2 % of
        // its distance (about 3 px at 2544 wide: the TAA jitter), so the image is the same.
        let side_cull = fn_live::var("FN_BACKDROP_FRUSTUM").map_or(true, |v| v != "0");
        let fwd = fn_fx::view::dir_to_ue(bv.forward).as_vec3().normalize();
        let right = fwd.cross(fn_fx::view::dir_to_ue(bv.up).as_vec3()).normalize();
        let up = right.cross(fwd).normalize();
        let (tx, ty) = bv.tan_half;
        let (sx, sy) = ((1. + tx * tx).sqrt(), (1. + ty * ty).sqrt());
        // LOD per placement (ComputeStaticMeshLOD, statics_render::static_mesh_lod); every LOD has a draw entry, only the chosen one is submitted
        for (mi, li, e, vs, binds, l2w) in &b.draws {
            let m = &b.meshes[*mi];
            let wb = m.bounds.transform_by(l2w);
            if side_cull {
                let d = Vec3::from(wb.origin) - eye;
                let r = wb.radius + d.length() * 0.002;
                let z = d.dot(fwd);
                if z + r <= 0. || d.dot(right).abs() > z * tx + r * sx || d.dot(up).abs() > z * ty + r * sy { continue; }
            }
            if *li !=crate::statics_render::static_mesh_lod(&m.screen_size, m.lods.len(), Vec3::from(wb.origin), wb.radius, eye, bv.tan_half, crate::statics_render::lod_scale()) { continue; }
            let (lod, en) = (&m.lods[*li], &b.entries[*e]);
            if written.insert(*e) {
                let packed = fn_fx::uniform::evaluate(&en.mat.uniform, &en.mat.overrides, t.world);
                let mut cb = packed.cb; cb.resize(cb.len().max(en.gpu.material_registers).max(1), [0.0; 4]);
                out.buffer_writes.push((en.gpu.material_buf, bytes(&cb)));
                out.buffer_writes.extend(en.gpu.per_frame_writes(&en.mat, t));
            }
            if let (Some(vb), Some(by)) = (vs, en.gpu.vs_globals_bytes(&en.mat, t, None)) { out.buffer_writes.push((*vb, by)); }
            for &(first, count) in &lod.sections {
                out.draws.push(FxDraw { pipe: en.gpu.pipe, binds: binds.clone(), geom: FxGeom::MeshRange { mesh: lod.mesh, first_index: first, index_count: count }, instances: None, sort: bv.sort(wb.origin) });
            }
            st.0 += 1; st.1 += lod.tris as u64;
        }
        self.stats = st;
        if let Ok(mut g) = RUN.lock() { g.0 += 1; g.1 += st.0 as u64; g.2 += st.1; }
    }
}

static STATE: Mutex<Option<BackdropReal>> = Mutex::new(None);

/// Opens the paks + reads `backdrop.json` (no-op without them or when disabled).
pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) {
    if !enabled() { return; }
    let (Some(install), Some(dir)) = (install, crate::backdrop_load::find_dir(roots)) else { eprintln!("[backdrop real] no install / backdrop folder: off"); return };
    match BackdropReal::new(&install, &dir) { Ok(s) => *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(s), Err(e) => eprintln!("[backdrop real] unavailable: {e}") }
}
/// The frame's backdrop draws (built on the first call).
pub fn frame(fx: &mut FxRenderer, view: &ViewParams, out: &mut FxFrame) {
    let Ok(mut g) = STATE.lock() else { return };
    if let Some(s) = g.as_mut() { s.frame(fx, view, out); }
}
pub fn prebuild(fx: &mut FxRenderer) {
    let Ok(mut g) = STATE.lock() else { return };
    let Some(s) = g.as_mut() else { return };
    if s.built.is_none() { let r = s.build(fx); if let Err(e) = &r { eprintln!("[backdrop real] NOT BUILT ({e}): the far backdrop is not drawn on the real-shader path"); } s.built = Some(r); }
}
/// The real path is on (backdrop_load then leaves the far skybox meshes out of the reconstructed `Kind::Skybox` draw).
pub fn active() -> bool { STATE.lock().ok().map_or(false, |g| g.as_ref().map_or(false, |s| !matches!(s.built, Some(Err(_))))) }
/// REPORT ONLY (2026-10-08): (frames, summed draws, summed triangles) of the far meshes over the run.
static RUN: Mutex<(u64, u64, u64)> = Mutex::new((0, 0, 0));
pub fn run_report() -> String {
    let Ok(g) = RUN.lock() else { return String::new() };
    if g.0 == 0 { return String::new(); }
    format!("backdrop real run average ({} frames): {:.2} far meshes / {:.0} triangles drawn\n", g.0, g.1 as f64 / g.0 as f64, g.2 as f64 / g.0 as f64)
}
