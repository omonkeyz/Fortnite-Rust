//! LANDSCAPE MATERIAL (2026-10-07): the Athena landscape components within the main pass drawn with their REAL cooked material programs (the
//! `LandscapeMaterialInstanceConstant` of each component: M_Athena_Terrain_Landscape's static permutation, compiled for `FLandscapeVertexFactory`) instead of
//! the reconstruction of terrain_common.wgsl `terrain_surface` (vertex-colour layer weights, assumed rock slope window / tiling, DiffuseColor1 tint).
//!
//! Same machinery as the statics (statics_render.rs): `fn_fx::material::Material::load` of the component's instance chain (the shader map of the child-most
//! instance inside the sublevel umap that owns programs, scoped to that export: fn_fx material.rs `shader_map_scoped`), `TBasePassVSFNoLightMapPolicy` /
//! `TBasePassPSFNoLightMapPolicy` translated from DXBC and drawn as scene-lit FX draws (the GBuffer the pixel program writes -> the renderer's lighting).
//! The landscape vertex factory (UE4.16 LandscapeVertexFactory.usf / LandscapeRender.cpp), ported to the FX path (fn_fx gpu.rs `Vf::Landscape`):
//! * vertex stream: `FLandscapeVertexBuffer::InitRHI` (LandscapeRender.cpp:1818): one (x, y, SubX, SubY) per vertex of the subsection grid; the LOD index
//!   buffers of `FLandscapeSharedBuffers::CreateIndexBuffers` (SM4+ branch, :1997): LOD L uses the first `(SubsectionSizeVerts >> L)` columns / rows,
//!   triangles (i00, i11, i10), (i00, i01, i11);
//! * `LandscapeParameters` per component (`OnTransformChanged`, :1017): HeightmapUVScaleBias, WeightmapUVScaleBias, (lightmap: unused by the no-lightmap
//!   programs), (SubsectionSizeVerts, 1 / SubsectionSizeQuads, SectionBase), (HeightmapSubsectionOffsetU/V = (SubsectionSizeQuads + 1) / heightmap size,
//!   WeightmapSubsectionOffset, SubsectionSizeQuads), LocalToWorldNoScaling;
//! * the VS loose values of `SetMesh` (:2328): LodBias (z = first resident heightmap mip: 0, every cooked mip is uploaded), LodValues, SectionLods and
//!   NeighborSectionLod from `CalcDesiredLOD` / `CalcLODParamsForSubsection` (:1290-1385, linear falloff, LANDSCAPE_LOD_DISTANCE_FACTOR 2, the proxy's
//!   LODDistanceFactor / MaxLODLevel), so the cooked VS morphs between LODs as 1.8 does;
//! * the heightmap texture (VS, `SF_Point`) and the same texture as `NormalmapTexture` (PS: the per-pixel normal from B / A; :535), weightmaps as the instance's
//!   `Weightmap0..` texture parameters.
//! Components beyond the main pass far plane stay on the backdrop pass (reconstruction, far); the CSM casters of the landscape stay on backdrop.rs.
//! `FN_LANDSCAPE_MATERIAL=0` = the old reconstruction (before/after pairs).
use fn_fx::gpu::{build_material_layout, LandscapeVfValues, LayoutOpts, MaterialGpu, SharedBuffers, TextureCache, Vf};
use fn_fx::material::Material;
use fn_fx::view::{bytes, sprite_vf_buffer, ViewParams, VIEW_FLOAT4S};
use fn_pak::PakSet;
use fn_render::fx::*;
use glam::{Mat4, Vec2, Vec3};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const VS_TYPE: &str = "TBasePassVSFNoLightMapPolicy";
const PS_TYPE: &str = "TBasePassPSFNoLightMapPolicy";
const MAP: &str = "/Game/Athena/Maps/Athena_Terrain";
/// LandscapeRender.cpp:33.
const LANDSCAPE_LOD_DISTANCE_FACTOR: f32 = 2.0;

/// BROWSER PORT: off by default in the browser build (`?FN_LANDSCAPE_MATERIAL=1` turns it on): its build is one main-thread step (94 materials + heightmaps:
/// 28 s of frozen loading screen, plus ~4 min of worker parsing on a cold start, verification/browser/remote-A-cold); it needs an incremental build first.
pub fn enabled() -> bool { fn_live::var("FN_LANDSCAPE_MATERIAL").map_or(true, |v| v != "0") }
fn prewarm_running() -> bool { PREWARM.lock().map_or(false, |g| g.as_ref().is_some_and(|h| !h.is_finished())) }

/// `animated`: the Material buffer or the per-frame globals change with time (written every frame), else written once at build.
struct Entry { mat: Arc<Material>, gpu: MaterialGpu, animated: bool }
struct Comp {
    name: String,
    l2w: Mat4,
    /// `CachedLocalBox` as bounds (local units: quads, height / 128).
    bounds: fn_render::primitive_ub::BoxSphereBounds,
    prim: FxBufId,
    land: FxBufId,
    vs: FxBufId,
    heightmap: FxTexId,
    normal_sampler: FxSamplerDesc,
    entry: usize,
    binds: Vec<(u32, FxBindId)>,
    /// PERF: the VS globals last uploaded (rewritten only when the LOD values change).
    last_vs: Option<Vec<u8>>,
}
struct Built {
    shared: SharedBuffers,
    comps: Vec<Comp>,
    entries: Vec<Entry>,
    mesh: FxMeshId,
    /// (first index, index count) per LOD.
    lods: Vec<(u32, u32)>,
    size_quads: u32,
    lod_distance: f32,
    dist_diff: f32,
    max_lod: u32,
    /// LOAD TIME (incremental build): the components not built yet (`None` = every component built: only then the path draws).
    pending: Option<Pending>,
}
/// LOAD TIME: what [`State::step`] still has to build: the cooked components from `next` on, the materials parsed by [`prewarm`] not taken yet.
struct Pending { cooked: Vec<Cooked>, next: usize, entry_of: std::collections::HashMap<String, usize>, pre_mats: std::collections::HashMap<String, Result<Arc<Material>, String>>, shift: Vec3, fade: FxBufId, t_ms: f32 }
struct State { set: Arc<PakSet>, cache: TextureCache, built: Option<Result<Built, String>>, stats: (usize, usize) }
static STATE: Mutex<Option<State>> = Mutex::new(None);

/// LOAD TIME (2026-10-07, both builds; the browser port needed it: there every read is a synchronous request): what [`prewarm`] read on a worker
/// thread while the match loads (the component list, the parsed materials; the heightmaps are predecoded in fn_fx), so the build on the main
/// thread ([`prebuild_step`], under the loading screen) only creates GPU objects. Same calls, same results: only where and when they run changes.
struct Preloaded { cooked: Option<Result<Vec<Cooked>, String>>, mats: std::collections::HashMap<String, Result<Arc<Material>, String>> }
static PRELOADED: Mutex<Option<Preloaded>> = Mutex::new(None);
static PREWARM: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

/// Starts reading the landscape's packages on a worker (after [`init`]; the world load must install the landscape for the build, not for this).
pub fn prewarm() {
    let Some(set) = STATE.lock().ok().and_then(|g| g.as_ref().map(|s| s.set.clone())) else { return };
    if PREWARM.lock().map_or(true, |g| g.is_some()) { return; }
    let h = std::thread::Builder::new().name("landscape prewarm".into()).spawn(move || {
        let t0 = std::time::Instant::now();
        let (cooked, mats, heightmaps) = fn_pak::retain_reads(|| {
            let cooked = cooked_components(&set);
            let mut paths: Vec<String> = vec![];
            let mut heightmaps: Vec<String> = vec![];
            for c in cooked.iter().flatten() {
                if let Some(m) = c.props.get("MaterialInstances").and_then(|v| v.as_array()).and_then(|a| a.first()).and_then(|v| v.as_str()) { if !paths.iter().any(|x| x == m) { paths.push(m.to_owned()); } }
                if let Some(h) = c.props.get("HeightmapTexture").and_then(|v| v.as_str()) { heightmaps.push(h.to_owned()); }
            }
            let next = std::sync::atomic::AtomicUsize::new(0);
            let out: Mutex<std::collections::HashMap<String, Result<Arc<Material>, String>>> = Mutex::new(Default::default());
            std::thread::scope(|sc| {
                for _ in 0..if cfg!(target_arch = "wasm32") { 2 } else { std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 6) } {
                    sc.spawn(|| loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(p) = paths.get(i) else { break };
                        let r = Material::load(&set, p).map(Arc::new);
                        if let Ok(mut o) = out.lock() { o.insert(p.clone(), r); }
                    });
                }
            });
            (cooked, out.into_inner().unwrap_or_default(), heightmaps)
        });
        for h in &heightmaps { fn_fx::gpu::predecode_texture(&set, h); }
        let mut textures: Vec<String> = vec![];
        for m in mats.values().flatten() { for k in 0..m.uniform.textures2d.len() { if let Some((p, _)) = m.texture_slot(k) { if !textures.contains(&p) { textures.push(p); } } } }
        let t1 = std::time::Instant::now();
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|sc| {
            for _ in 0..if cfg!(target_arch = "wasm32") { 2 } else { 4 } {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(path) = textures.get(i) else { break };
                    fn_fx::gpu::predecode_texture(&set, path);
                });
            }
        });
        eprintln!("[landscape] prewarm: {} of {} material textures decoded in {:.0} ms", textures.iter().filter(|t| fn_fx::gpu::is_predecoded(t)).count(), textures.len(), t1.elapsed().as_secs_f32() * 1000.);
        eprintln!("[landscape] prewarm: {} components, {} materials parsed, {} heightmaps decoded in {:.0} ms", cooked.as_ref().map_or(0, |c| c.len()), mats.len(), heightmaps.len(), t0.elapsed().as_secs_f32() * 1000.);
        if let Ok(mut g) = PRELOADED.lock() { *g = Some(Preloaded { cooked: Some(cooked), mats }); }
    });
    if let (Ok(h), Ok(mut g)) = (h, PREWARM.lock()) { *g = Some(h); }
}

/// LOADING SCREEN: builds the landscape's GPU data once [`prewarm`] is done (so the first frame of play does not); true when built or not needed.
pub fn prebuild_step(fx: &mut FxRenderer) -> bool {
    if prewarm_running() { return false; }
    if fn_terrain::world().is_none() { return true; }
    let Ok(mut g) = STATE.lock() else { return true };
    let Some(s) = g.as_mut() else { return true };
    // LOAD TIME: about 8 ms of building per loading-screen step
    s.ensure(fx, Some(std::time::Instant::now() + std::time::Duration::from_millis(8)))
}

/// Opens the paks (GPU data is made on the first frame). No-op when disabled.
pub fn init(install: Option<PathBuf>) {
    if !enabled() { return; }
    let Some(install) = install else { return };
    if STATE.lock().map_or(true, |g| g.is_some()) { return; }
    match PakSet::open_install(&install) {
        Ok(set) => { *STATE.lock().unwrap() = Some(State { set: Arc::new(set), cache: TextureCache::default(), built: None, stats: (0, 0) }); }
        Err(e) => eprintln!("[landscape] paks: {e}: the landscape keeps the reconstruction"),
    }
}

fn f(v: Option<&serde_json::Value>, d: f32) -> f32 { v.and_then(|x| x.as_f64()).map_or(d, |x| x as f32) }
fn f4(v: Option<&serde_json::Value>, d: [f32; 4]) -> [f32; 4] { v.and_then(|x| x.as_array()).map_or(d, |a| [f(a.first(), d[0]), f(a.get(1), d[1]), f(a.get(2), d[2]), f(a.get(3), d[3])]) }

/// One cooked LandscapeComponent (props read from its sublevel), in the exporter's order (fn-export props/backdrop.rs `gather_terrain`: the levels of the
/// persistent map in order, each level's landscape components in export order) = the order of `terrain.bin` / `fn_terrain`'s components.
struct Cooked { level: String, name: String, props: serde_json::Value, proxy: serde_json::Value }

fn cooked_components(set: &PakSet) -> Result<Vec<Cooked>, String> {
    let keep = |c: &str| c.starts_with("LevelStreaming") || c == "Landscape" || c == "LandscapeStreamingProxy" || c == "LandscapeComponent";
    let persistent = fn_pak::without_retain(|| fn_fx::data::PackageJson::load_classes(set, MAP, keep))?;
    let mut levels = vec![MAP.to_owned()];
    for e in persistent.exports.iter().filter(|e| e.class.starts_with("LevelStreaming")) {
        if let Some(wa) = e.props.get("WorldAsset").and_then(|v| v.as_str()) { levels.push(fn_fx::data::long_package(wa).to_owned()); }
    }
    let mut out = vec![];
    for lv in levels {
        let pkg = if lv == MAP { None } else { Some(fn_pak::without_retain(|| fn_fx::data::PackageJson::load_classes(set, &lv, keep))?) };
        let pkg = pkg.as_ref().unwrap_or(&persistent);
        let proxy = pkg.exports.iter().find(|e| e.class == "Landscape" || e.class == "LandscapeStreamingProxy").map(|e| e.props.clone()).unwrap_or_default();
        if pkg.exports.iter().filter(|e| e.class == "Landscape" || e.class == "LandscapeStreamingProxy").count() > 1 { return Err(format!("{lv}: more than one landscape actor (component order not resolved)")); }
        for e in pkg.of_class("LandscapeComponent") { out.push(Cooked { level: lv.clone(), name: e.name.clone(), props: e.props.clone(), proxy: proxy.clone() }); }
    }
    Ok(out)
}

impl State {
    /// Builds (incrementally) until `deadline`; true when the path is ready or has failed (nothing more to build).
    fn ensure(&mut self, fx: &mut FxRenderer, deadline: Option<std::time::Instant>) -> bool {
        if self.built.is_none() { self.built = Some(self.begin(fx)); }
        let mut b = match self.built.take() { Some(Ok(b)) => b, other => { if let Some(Err(e)) = &other { static W: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false); if !W.swap(true, std::sync::atomic::Ordering::Relaxed) { eprintln!("[landscape] NOT ON THE REAL MATERIAL ({e}): the landscape keeps the reconstruction"); } } self.built = other; return true; } };
        let r = self.step(fx, &mut b, deadline);
        match r {
            Ok(done) => { self.built = Some(Ok(b)); done }
            Err(e) => { eprintln!("[landscape] NOT ON THE REAL MATERIAL ({e}): the landscape keeps the reconstruction"); self.built = Some(Err(e)); true }
        }
    }
    /// The shared GPU data (buffers, the subsection mesh, LOD settings) and the component list; the components themselves are built by [`Self::step`].
    fn begin(&mut self, fx: &mut FxRenderer) -> Result<Built, String> {
        let t0 = std::time::Instant::now();
        let world = fn_terrain::world().ok_or("no landscape installed (backdrop_load)")?;
        let tcomps = &world.hf.set.comps;
        let pre = PRELOADED.lock().ok().and_then(|mut g| g.take());
        let (cooked, pre_mats) = match pre { Some(Preloaded { cooked: Some(c), mats }) => (c?, mats), Some(Preloaded { mats, .. }) => (cooked_components(&self.set)?, mats), None => (cooked_components(&self.set)?, Default::default()) };
        if cooked.len() != tcomps.len() { return Err(format!("{} cooked components vs {} in terrain.bin", cooked.len(), tcomps.len())); }
        // FX space = UE world cm translated: to_ue(map.to_game(ue)) = ue - shift (backdrop.rs to_game, fn_fx view.rs to_ue)
        let m = &world.map;
        let shift = Vec3::new(m.centre_cm[0] - m.anchor.x * 100., m.centre_cm[1] - m.anchor.z * 100., m.ref_z_cm);
        let view = fx.create_buffer("landscape view", VIEW_FLOAT4S * 16);
        let light = fx.create_buffer("landscape light", fn_render::sky_light::FX_LIGHT_BYTES);
        let sprite_vf = fx.create_buffer("landscape sprite vf", 10 * 16);
        fx.write_buffer(sprite_vf, &bytes(&sprite_vf_buffer()));
        let prim0 = fx.create_buffer("landscape primitive (shared)", 32 * 16);
        let fade = fx.create_buffer("landscape PrimitiveFade", 16);
        fx.write_buffer(fade, &bytes(&fn_matshader::frame::fade_buffer()));
        let shared = SharedBuffers { view, primitive: prim0, sprite_vf, light };
        // every component must share one subsection layout (one vertex / index buffer, one subsection per component)
        let sq = cooked[0].props.get("SubsectionSizeQuads").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        for c in &cooked {
            let (s, n) = (c.props.get("SubsectionSizeQuads").and_then(|v| v.as_u64()).unwrap_or(0) as u32, c.props.get("NumSubsections").and_then(|v| v.as_u64()).unwrap_or(1));
            if s != sq || n != 1 { return Err(format!("{} {}: SubsectionSizeQuads {s} NumSubsections {n} (only one {sq}-quad subsection per component is handled)", c.level, c.name)); }
        }
        let sv = sq + 1;
        let mut verts: Vec<u8> = Vec::with_capacity((sv * sv) as usize * 16);
        for y in 0..sv { for x in 0..sv { for v in [x as f32, y as f32, 0., 0.] { verts.extend_from_slice(&v.to_le_bytes()); } } }
        let mut indices: Vec<u32> = vec![];
        let mut lods = vec![];
        let max_lod = (32 - (sv - 1).leading_zeros()).saturating_sub(1); // CeilLogTwo(SubsectionSizeVerts) - 1
        for l in 0..=max_lod {
            let q = (sv >> l) - 1;
            let first = indices.len() as u32;
            for y in 0..q { for x in 0..q {
                let (i00, i10, i11, i01) = (x + y * sv, x + 1 + y * sv, x + 1 + (y + 1) * sv, x + (y + 1) * sv);
                indices.extend_from_slice(&[i00, i11, i10, i00, i01, i11]);
            } }
            lods.push((first, indices.len() as u32 - first));
        }
        let mesh = fx.create_mesh(&FxMeshDesc { label: "landscape subsection".into(), vertices: verts, indices });
        // the proxy's LOD settings (ALandscapeProxy defaults: LODDistanceFactor 1, LODFalloff Linear, MaxLODLevel -1)
        let proxy = &cooked[0].proxy;
        let ldf = f(proxy.get("LODDistanceFactor"), 1.0);
        let falloff = proxy.get("LODFalloff").and_then(|v| v.as_str()).unwrap_or("ELandscapeLODFalloff::Linear").to_owned();
        let max_level = proxy.get("MaxLODLevel").and_then(|v| v.as_i64()).unwrap_or(-1);
        let max_lod = if max_level >= 0 { max_lod.min(max_level as u32) } else { max_lod };
        let factor = if falloff.ends_with("SquareRoot") { (1.5 * ldf).min(10.).powi(2) } else { ldf };
        let lod_distance = (2.0 * (sq as f32).powi(2)).sqrt() * LANDSCAPE_LOD_DISTANCE_FACTOR / factor;
        let dist_diff = -(2.0 * (0.5 * sq as f32).powi(2)).sqrt();
        eprintln!("[landscape] {} components, subsection {sq} quads, LODs 0..{max_lod}, LODDistanceFactor {ldf} {falloff} (LODDistance {lod_distance:.1} quads)", cooked.len());
        if falloff.ends_with("SquareRoot") { eprintln!("[landscape] SquareRoot falloff: fLOD uses sqrt (CalcDesiredLOD)"); }
        // the landscape origins agree (order check of the component matching)
        let o0: Vec<Vec2> = cooked.iter().zip(tcomps).map(|(c, t)| Vec2::new(t.origin[0] - f(c.props.get("SectionBaseX"), 0.) * t.step[0], t.origin[1] - f(c.props.get("SectionBaseY"), 0.) * t.step[1])).collect();
        let spread = o0.iter().map(|o| (*o - o0[0]).length()).fold(0f32, f32::max);
        if spread > 1.0 { return Err(format!("component order mismatch: landscape origins differ by {spread:.1} cm")); }
        let n = cooked.len();
        let pending = Pending { cooked, next: 0, entry_of: Default::default(), pre_mats, shift, fade, t_ms: t0.elapsed().as_secs_f32() * 1000. };
        Ok(Built { shared, comps: Vec::with_capacity(n), entries: vec![], mesh, lods, size_quads: sq, lod_distance, dist_diff, max_lod, pending: Some(pending) })
    }

    /// LOAD TIME (incremental build): builds components until `deadline` (at least one; `None` = all). True when every component is built.
    fn step(&mut self, fx: &mut FxRenderer, b: &mut Built, deadline: Option<std::time::Instant>) -> Result<bool, String> {
        let t0 = std::time::Instant::now();
        let world = fn_terrain::world().ok_or("no landscape installed (backdrop_load)")?;
        let tcomps = &world.hf.set.comps;
        let Some(p) = b.pending.as_mut() else { return Ok(true) };
        let (shift, fade, shared, sq) = (p.shift, p.fade, b.shared, b.size_quads);
        let sv = sq + 1;
        let (entries, comps) = (&mut b.entries, &mut b.comps);
        let first = p.next;
        while p.next < p.cooked.len() {
            if p.next > first && deadline.is_some_and(|d| std::time::Instant::now() >= d) { break; }
            let ci = p.next;
            let c = &p.cooked[ci];
            let tc = &tcomps[ci];
            let (entry_of, pre_mats) = (&mut p.entry_of, &mut p.pre_mats);
            let mat_path = c.props.get("MaterialInstances").and_then(|v| v.as_array()).and_then(|a| a.first()).and_then(|v| v.as_str()).ok_or_else(|| format!("{}: no MaterialInstances", c.name))?.to_owned();
            let entry = match entry_of.get(&mat_path) { Some(&e) => e, None => {
                let mat = match pre_mats.remove(&mat_path) { Some(m) => m?, None => Arc::new(Material::load(&self.set, &mat_path)?) };
                let gpu = build_material_layout(&self.set, fx, &mut self.cache, &mat, Vf::Landscape, VS_TYPE, PS_TYPE, 0, false, shared, &[("PrimitiveFade", fade)], LayoutOpts::default())?;
                if !gpu.has_landscape_slots() { return Err(format!("{mat_path}: no landscape vertex factory bindings in its programs")); }
                let tex: Vec<String> = (0..mat.uniform.textures2d.len()).map(|k| mat.texture_slot(k).map_or("NULL".into(), |t| t.0.rsplit('.').next().unwrap_or("").to_owned())).collect();
                eprintln!("[landscape] material {} (shader map of {}): {VS_TYPE} / {PS_TYPE}, {} vectors / {} scalars, textures {tex:?}", mat_path.rsplit('.').next().unwrap_or(""), mat.shader_map_package.rsplit('/').next().unwrap_or(""), mat.uniform.vectors.len(), mat.uniform.scalars.len());
                // MEMORY: the programs are translated (gpu.assembled) and the per-frame writes read only the uniform set: the instance's ~240 SM5 programs
                // (94 instances, each its own static-permutation map) are dropped here instead of living as long as the landscape
                let mat = Arc::new(Material { cooked: Arc::new(mat.cooked.subset(0, 0)), ..(*mat).clone() });
                // PERF: time-independent uniforms (the Material buffer + per-frame globals equal at two times) are written once here, not per frame
                let at = |t: f32| { let tt = fn_fx::uniform::MaterialTime { world: t, real: t, delta_world: 0. }; (fn_fx::uniform::evaluate(&mat.uniform, &mat.overrides, t).cb, gpu.per_frame_writes(&mat, tt).into_iter().map(|w| w.1).collect::<Vec<_>>()) };
                let animated = at(0.) != at(37.25);
                { let packed = fn_fx::uniform::evaluate(&mat.uniform, &mat.overrides, 0.); let mut cb = packed.cb; cb.resize(cb.len().max(gpu.material_registers).max(1), [0.0; 4]); fx.write_buffer(gpu.material_buf, &bytes(&cb)); for (bf, by) in gpu.per_frame_writes(&mat, fn_fx::uniform::MaterialTime::default()) { fx.write_buffer(bf, &by); } }
                entries.push(Entry { mat, gpu, animated });
                entry_of.insert(mat_path.clone(), entries.len() - 1);
                entries.len() - 1
            } };
            let hm_path = c.props.get("HeightmapTexture").and_then(|v| v.as_str()).ok_or_else(|| format!("{}: no HeightmapTexture", c.name))?.to_owned();
            let (heightmap, info) = self.cache.load(&self.set, fx, &hm_path)?;
            if info.srgb { return Err(format!("{hm_path}: sRGB heightmap")); }

            let addr = if info.address_x == "TA_Wrap" { FxAddress::Wrap } else { FxAddress::Clamp };
            // (WebGPU: anisotropy needs linear mip filtering; the group's point mip is kept, so no anisotropy)
            let normal_sampler = FxSamplerDesc { mag: FxFilter::Linear, min: FxFilter::Linear, mip: FxFilter::Nearest, address: addr, aniso: 1 };
            let hsb = f4(c.props.get("HeightmapScaleBias"), [1. / sv as f32, 1. / sv as f32, 0., 0.]);
            let wsb = f4(c.props.get("WeightmapScaleBias"), [0.; 4]);
            let wso = f(c.props.get("WeightmapSubsectionOffset"), 0.);
            let base = [f(c.props.get("SectionBaseX"), 0.), f(c.props.get("SectionBaseY"), 0.)];
            // consistency with terrain.bin: the component's origin minus SectionBase * scale is the landscape origin (same for every component of one level)
            let scale = Vec3::new(tc.step[0], tc.step[1], tc.step[2] * 128.);
            let origin = Vec3::from(tc.origin);
            let l2w = Mat4::from_translation(origin - shift) * Mat4::from_scale(scale);
            let land: Vec<[f32; 4]> = vec![hsb, wsb, [0.; 4], [sv as f32, 1. / sq as f32, base[0], base[1]], [sv as f32 / info.width as f32, sv as f32 / info.height as f32, wso, sq as f32], [0.; 4],
                [1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.], [origin.x - shift.x, origin.y - shift.y, origin.z - shift.z, 1.]];
            let land_buf = fx.create_buffer("landscape parameters", land.len() * 16);
            fx.write_buffer(land_buf, &bytes(&land));
            let lb = c.props.get("CachedLocalBox");
            let (lo, hi) = (lb.and_then(|b| b.get("Min")).map_or([0., 0., -256.], |v| { let a = f4(Some(v), [0.; 4]); [a[0], a[1], a[2]] }), lb.and_then(|b| b.get("Max")).map_or([sq as f32, sq as f32, 256.], |v| { let a = f4(Some(v), [0.; 4]); [a[0], a[1], a[2]] }));
            let ext = [(hi[0] - lo[0]) * 0.5, (hi[1] - lo[1]) * 0.5, (hi[2] - lo[2]) * 0.5];
            let bounds = fn_render::primitive_ub::BoxSphereBounds { origin: [(hi[0] + lo[0]) * 0.5, (hi[1] + lo[1]) * 0.5, (hi[2] + lo[2]) * 0.5], extent: ext, radius: Vec3::from(ext).length() };
            let prim = fx.create_buffer("landscape primitive", 32 * 16);
            fx.write_buffer(prim, &bytes(&fn_render::primitive_ub::mesh_primitive(&l2w, &bounds, (origin - shift).to_array(), &fn_render::primitive_ub::PrimitiveFlags::default())));
            let n = entries[entry].gpu.vs_globals().map_or(16, |g| g.float4s);
            let vs = fx.create_buffer("landscape VS globals", n * 16);
            let binds = entries[entry].gpu.bind_variant_landscape(fx, prim, None, Some(vs), land_buf, heightmap, normal_sampler);
            if ci < 2 { eprintln!("[landscape] {} {}: material {}, heightmap {} {}x{} mips {}, SectionBase {base:?}, HeightmapScaleBias {hsb:?}, WeightmapScaleBias {wsb:?}, origin {:?}", c.level.rsplit('/').next().unwrap_or(""), c.name, mat_path.rsplit('.').next().unwrap_or(""), hm_path.rsplit('.').next().unwrap_or(""), info.width, info.height, info.levels, tc.origin); }
            comps.push(Comp { name: format!("{} {}", c.level.rsplit('/').next().unwrap_or(""), c.name), l2w, bounds, prim, land: land_buf, vs, heightmap, normal_sampler, entry, binds, last_vs: None });
            p.next += 1;
        }
        p.t_ms += t0.elapsed().as_secs_f32() * 1000.;
        if p.next < p.cooked.len() { return Ok(false); }
        fn_fx::material::drop_map_programs();
        let pipes: std::collections::HashSet<usize> = entries.iter().map(|e| e.gpu.pipe.0).collect();
        eprintln!("[landscape] built {} components / {} materials ({} distinct pipelines, {} animated) in {:.0} ms (build steps)", comps.len(), entries.len(), pipes.len(), entries.iter().filter(|e| e.animated).count(), p.t_ms);
        b.pending = None;
        Ok(true)
    }
}

impl Built {
    /// `CalcDesiredLOD` (LandscapeRender.cpp:1290, perspective, linear falloff, no forced LOD / bias): camera in component local units (quads).
    fn desired_lod(&self, cam: Vec2, sub: (i32, i32)) -> f32 {
        let s = self.size_quads as f32;
        let p = cam - Vec2::new(sub.0 as f32 * s, sub.1 as f32 * s);
        let d = (p - Vec2::splat(0.5 * s)).length() + self.dist_diff;
        (d / self.lod_distance).clamp(0., self.max_lod as f32)
    }
    fn vf_values(&self, cam: Vec2) -> (u32, LandscapeVfValues) {
        let lod = self.desired_lod(cam, (0, 0));
        let cur = lod.floor() as u32;
        let f = (cur as f32).max(lod);
        let nb = [(0, -1), (-1, 0), (1, 0), (0, 1)].map(|s| f.max(self.desired_lod(cam, s)));
        let q = ((self.size_quads + 1) >> cur) as f32 - 1.;
        let mut v = LandscapeVfValues { lod_bias: [0.; 4], lod_values: [cur as f32, 0., q, 1. / q], section_lods: [f, 0., 0., 0.], neighbor_section_lod: [[0.; 4]; 4] };
        v.neighbor_section_lod[0] = nb;
        (cur, v)
    }
}

/// The frame of the landscape components within the main pass. `bounds` = the backdrop's component bounds (game m, component order: the same
/// `distance + radius < far` test the backdrop uses to hand a component to the main pass). Returns true when this path draws the main-pass landscape
/// (the caller then turns the backdrop's main-pass landscape off).
pub fn frame(fx: &mut FxRenderer, view: &ViewParams, bounds: &[(Vec3, f32)], out: &mut FxFrame) -> bool {
    let Ok(mut g) = STATE.lock() else { return false };
    let Some(s) = g.as_mut() else { return false };
    // LOAD TIME: whatever the loading screen left: all of it natively (scripted runs draw the first frame complete), ~4 ms per frame in the browser
    if cfg!(target_arch = "wasm32") && (prewarm_running() || PREWARM.lock().map_or(true, |g| g.is_none())) { return false; }
    let deadline = if cfg!(target_arch = "wasm32") { Some(std::time::Instant::now() + std::time::Duration::from_millis(4)) } else { None };
    if !s.ensure(fx, deadline) { return false; }
    let Some(Ok(b)) = s.built.as_mut() else { return false };
    if bounds.len() != b.comps.len() { static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false); if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) { eprintln!("[landscape] the backdrop has {} landscape meshes, this path {} components: not drawn", bounds.len(), b.comps.len()); } return false; }
    out.buffer_writes.push((b.shared.view, bytes(&view.view_buffer())));
    out.buffer_writes.push((b.shared.light, bytes(&fn_render::sky_light::fx_light_block())));
    let t = view.material_time();
    let eye = fn_fx::view::to_ue(view.eye).as_vec3();
    let fwd = fn_fx::view::dir_to_ue(view.forward).as_vec3().normalize();
    let up0 = fn_fx::view::dir_to_ue(view.up).as_vec3();
    let right = fwd.cross(up0).normalize();
    let up = right.cross(fwd).normalize();
    let (tx, ty) = view.tan_half;
    let (sx, sy) = ((1. + tx * tx).sqrt(), (1. + ty * ty).sqrt());
    let mut written = vec![false; b.entries.len()];
    let mut drawn = 0;
    for ci in 0..b.comps.len() {
        let c = &b.comps[ci];
        let (bc, br) = bounds[ci];
        if (bc - view.eye).length() + br >= view.far_m { continue; } // the backdrop pass draws it (far)
        let wb = c.bounds.transform_by(&c.l2w);
        let (o, r) = (Vec3::from(wb.origin), wb.radius);
        let d = o - eye;
        let z = d.dot(fwd);
        if !(z + r > view.near_m * 100. && d.dot(right).abs() <= z * tx + r * sx && d.dot(up).abs() <= z * ty + r * sy) { continue; }
        let e = &b.entries[c.entry];
        if e.animated && !written[c.entry] {
            written[c.entry] = true;
            let packed = fn_fx::uniform::evaluate(&e.mat.uniform, &e.mat.overrides, t.world);
            let mut cb = packed.cb; cb.resize(cb.len().max(e.gpu.material_registers).max(1), [0.0; 4]);
            out.buffer_writes.push((e.gpu.material_buf, bytes(&cb)));
            out.buffer_writes.extend(e.gpu.per_frame_writes(&e.mat, t));
        }
        // camera in component local units (WorldToLocal of the unrotated component: translation + scale)
        let cl = c.l2w.inverse().transform_point3(eye);
        let (lod, vf) = b.vf_values(Vec2::new(cl.x, cl.y));
        let vs_bytes = e.gpu.vs_globals_bytes_landscape(&e.mat, t, None, &vf);
        let (first, count) = b.lods[(lod as usize).min(b.lods.len() - 1)];
        let pipe = e.gpu.pipe;
        let c = &mut b.comps[ci];
        if let Some(by) = vs_bytes { if c.last_vs.as_ref() != Some(&by) { out.buffer_writes.push((c.vs, by.clone())); c.last_vs = Some(by); } } // PERF: only when the LOD values changed
        out.draws.push(FxDraw { pipe, binds: c.binds.clone(), geom: FxGeom::MeshRange { mesh: b.mesh, first_index: first, index_count: count }, instances: None, sort: view.sort(wb.origin) });
        drawn += 1;
    }
    if fn_live::var_os("FN_LANDSCAPE_LOG").is_some() { eprintln!("[landscape] frame: {drawn} of {} components drawn (far {:.0} m)", b.comps.len(), view.far_m); }
    s.stats = (drawn, b.comps.len());
    true
}

/// (components drawn last frame, components on this path).
pub fn stats() -> (usize, usize) { STATE.lock().ok().and_then(|g| g.as_ref().map(|s| s.stats)).unwrap_or((0, 0)) }
