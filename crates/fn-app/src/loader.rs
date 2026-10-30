//! The in-match loading flow: real load work drives the loading screen's progress bar without ever blocking the UI thread.
//!
//! * **Worker side** (`build_world`, one thread plus scoped helpers): mount the paks (pak mode) -> character + pickaxe models -> every animation clip on a
//!   pool of worker threads -> ground texture + the whole `Renderer` (shader modules, pipelines, targets, sky: ONE call into the driver per phase, on its own
//!   thread so the window keeps animating) -> character textures -> upload the models into the renderer -> the finished `Scene`.
//! * **UI-thread side** (`Loader::poll`, called once per frame with a time budget): the work that needs the `UiLayer` (HUD art, menu brush textures, the
//!   offscreen warm-up render of the HUD / Esc menu / Settings so their pipelines exist before the first real frame) is cut into small resumable slices.
//!   A slice that has to compile a pipeline is the only thing that can run longer than the budget (the loader records the longest slice).
//! * **Progress** is a set of weighted units ([`Progress`]): every unit is announced BEFORE it runs (`begin`), timed (`end`) and counts for its
//!   weight (estimated seconds of work, calibrated from the timings the log prints); like the user's reference loading screen the bar moves only when a step
//!   COMPLETES (a running unit counts for nothing), never goes backwards and only reaches 100 % when everything is done.
use crate::assets::AssetRoots;
use crate::scene::{make_rigid, part_primitives, Character, Scene};
use crate::ui::UiLayer;
use fn_animgraph::ClipLibrary;
use fn_legacy::Md5Clip;
use fn_pose::{CharacterRig, ClipSourceDir, ClipStore, FnClip, Part, SampleMode};
use fn_propdata::PropCatalog;
use fn_render::{MaterialKind, Renderer, RendererAssets, SkyAssets};
use fn_ui::draw::{Cmd, RenderList};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub enum Source { Dir, Paks(PathBuf) }

enum Loaded { Md5(Md5Clip), Fn(Box<FnClip>) }

// ------------------------------------------------------------------------------------------------------------------------------------------
// progress
// ------------------------------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Unit { pub label: String, pub weight: f32, pub started: Option<f32>, pub finished: Option<f32> }
impl Unit { pub fn secs(&self) -> Option<f32> { Some(self.finished? - self.started?) } }

/// Pure progress bookkeeping on an explicit clock (seconds), so it can be tested without sleeping.
#[derive(Clone, Debug, Default)]
pub struct Core { pub units: Vec<Unit>, shown: f32, last_label: String }
/// The fraction of its weight a running unit may contribute before it finishes. 0 = the bar moves only when a step COMPLETES: the user's reference screens are driven
/// by completed load steps (`Progress { total, done }`), never by wall-clock time (the earlier 0.9 interpolation was an invention).
const RUNNING_CAP: f32 = 0.;
/// While anything is unfinished the bar stays below this.
const NOT_DONE_CAP: f32 = 0.995;
impl Core {
    pub fn new(units: Vec<(String, f32)>) -> Core { Core { units: units.into_iter().map(|(label, weight)| Unit { label, weight: weight.max(1e-4), started: None, finished: None }).collect(), shown: 0., last_label: String::new() } }
    pub fn begin(&mut self, i: usize, now: f32) { if let Some(u) = self.units.get_mut(i) { if u.started.is_none() { u.started = Some(now); self.last_label = u.label.clone(); } } }
    pub fn end(&mut self, i: usize, now: f32) { if let Some(u) = self.units.get_mut(i) { if u.started.is_none() { u.started = Some(now); } if u.finished.is_none() { u.finished = Some(now); } } }
    pub fn all_done(&self) -> bool { self.units.iter().all(|u| u.finished.is_some()) }
    pub fn total_weight(&self) -> f32 { self.units.iter().map(|u| u.weight).sum() }
    /// Monotonic fraction 0..1 of the work done at `now`.
    pub fn fraction(&mut self, now: f32) -> f32 {
        let total = self.total_weight().max(1e-6);
        let done: f32 = self.units.iter().map(|u| match (u.started, u.finished) {
            (_, Some(_)) => u.weight,
            (Some(s), None) => u.weight * ((now - s).max(0.) / u.weight).min(RUNNING_CAP),
            _ => 0.,
        }).sum();
        let f = if self.all_done() { 1. } else { (done / total).min(NOT_DONE_CAP) };
        self.shown = self.shown.max(f);
        self.shown
    }
    /// The unit announced last (what the screen says), or `Starting` before anything began.
    pub fn label(&self) -> String { if self.last_label.is_empty() { "Starting".to_owned() } else { self.last_label.clone() } }
    pub fn index_of(&self, label: &str) -> Option<usize> { self.units.iter().position(|u| u.label == label) }
}

/// Thread-safe progress shared between the worker threads and the UI thread.
pub struct Progress { core: Mutex<Core>, t0: Instant }
impl Progress {
    pub fn new(units: Vec<(String, f32)>) -> Arc<Progress> { Arc::new(Progress { core: Mutex::new(Core::new(units)), t0: Instant::now() }) }
    fn now(&self) -> f32 { self.t0.elapsed().as_secs_f32() }
    fn with<R>(&self, f: impl FnOnce(&mut Core, f32) -> R) -> R { let now = self.now(); let mut g = self.core.lock().unwrap_or_else(|e| e.into_inner()); f(&mut g, now) }
    pub fn begin(&self, i: usize) { self.with(|c, t| c.begin(i, t)) }
    pub fn end(&self, i: usize) { self.with(|c, t| c.end(i, t)) }
    pub fn fraction(&self) -> f32 { self.with(|c, t| c.fraction(t)) }
    pub fn label(&self) -> String { self.with(|c, _| c.label()) }
    pub fn all_done(&self) -> bool { self.with(|c, _| c.all_done()) }
    pub fn index_of(&self, label: &str) -> Option<usize> { self.with(|c, _| c.index_of(label)) }
    /// LOAD COUNTER: (finished units, all units).
    pub fn counts(&self) -> (usize, usize) { self.with(|c, _| (c.units.iter().filter(|u| u.finished.is_some()).count(), c.units.len())) }
    /// FRONT END: unit `i` finished (a missing unit counts as finished).
    pub fn is_finished(&self, i: usize) -> bool { self.with(|c, _| c.units.get(i).map_or(true, |u| u.finished.is_some())) }
    /// (label, seconds, start offset) of every finished unit, in the order they started.
    pub fn timings(&self) -> Vec<(String, f32, f32)> {
        self.with(|c, _| { let mut v: Vec<_> = c.units.iter().filter_map(|u| Some((u.label.clone(), u.secs()?, u.started?))).collect(); v.sort_by(|a, b| a.2.total_cmp(&b.2)); v })
    }
    pub fn elapsed(&self) -> f32 { self.now() }
}

/// Phases `Renderer::new_with_progress` announces (labels must match fn-render/src/renderer.rs) with their estimated weights in seconds.
pub const RENDERER_PHASES: [(&str, f32); 10] = [
    ("Creating bind group layouts", 0.02), ("Compiling shader modules", 0.15), ("Compiling shaders: ground, character and prop pipelines", 0.55),
    ("Compiling shaders: shadow pipelines", 0.25), ("Compiling shaders: post-processing pipelines", 0.55), ("Creating render buffers and shadow maps", 0.10),
    ("Uploading the ground texture", 0.10), ("Compiling shaders: sky", 0.80), ("Starting the GPU profiler", 0.05), ("Compiling shaders: textured static meshes", 0.30),
];
/// Estimated seconds per animation clip, per pak / tree texture etc. (calibrated from a measured run, see the log).
const W_PAKS: f32 = 0.15;
const W_MODELS: f32 = 0.10;
const W_CLIP: f32 = 0.02;
const W_TEXTURE: f32 = 0.10;
const W_CHAR_TEX: f32 = 0.35;
const W_ASSEMBLE: f32 = 0.30;
const W_HUD: f32 = 0.10;
const W_BRUSHES: f32 = 0.20;
const W_WARM: f32 = 0.45;
/// Estimated seconds to upload one real prop kind (textures + mips + mesh), calibrated from the load log.
const W_PROP: f32 = 0.04;
/// FOLIAGE HOOK: estimated seconds to read the 15.6 MB instance file, chunk it and upload the mesh + texture.
const W_FOLIAGE: f32 = 0.15;

/// Indices of the units in the [`Progress`] (the plan).
#[derive(Clone, Debug)]
pub struct Plan { pub paks: Option<usize>, pub loadout: Option<usize>, pub models: usize, pub clips: Vec<usize>, pub texture: usize, pub renderer: Vec<usize>, pub char_tex: usize, pub assemble: usize, pub hud: usize, pub brushes: usize, pub warm: usize, pub props: Vec<usize>, pub foliage: Option<usize> }

pub fn plan(source: &Source, clip_names: &[String]) -> (Vec<(String, f32)>, Plan) { plan_with_props(source, clip_names, &[]) }

/// The plan with one unit per real prop kind (`prop_ids`, in `props::loadable_kinds` order) between the renderer phases and the character upload: the unit of a kind
/// covers its GPU upload (its decode runs on a pool in parallel with the renderer's shader compilation and is not a unit of its own).
pub fn plan_with_props(source: &Source, clip_names: &[String], prop_ids: &[String]) -> (Vec<(String, f32)>, Plan) { plan_with_foliage(source, clip_names, prop_ids, None) }

/// FOLIAGE HOOK: [`plan_with_props`] plus one unit for the ambient ground cover (`Some(label)`) right after the prop kinds.
pub fn plan_with_foliage(source: &Source, clip_names: &[String], prop_ids: &[String], foliage: Option<String>) -> (Vec<(String, f32)>, Plan) {
    let mut u: Vec<(String, f32)> = vec![];
    let mut add = |label: String, w: f32| { u.push((label, w)); u.len() - 1 };
    let paks = matches!(source, Source::Paks(_)).then(|| add("Mounting the pak files".into(), W_PAKS));
    let loadout = matches!(source, Source::Paks(_)).then(|| add("Loading original starter weapons and animations".into(), 0.5));
    let models = add("Loading character and pickaxe models".into(), W_MODELS);
    let clips = clip_names.iter().map(|n| add(format!("Loading animation: {n}"), W_CLIP)).collect();
    let texture = add("Loading ground texture".into(), W_TEXTURE);
    let renderer = RENDERER_PHASES.iter().map(|(l, w)| add((*l).to_owned(), *w)).collect();
    let props = prop_ids.iter().map(|id| add(format!("Loading prop: {id}"), W_PROP)).collect();
    let foliage = foliage.map(|label| add(label, W_FOLIAGE)); // FOLIAGE HOOK
    let char_tex = add("Loading character textures".into(), W_CHAR_TEX);
    let assemble = add("Uploading character models".into(), W_ASSEMBLE);
    let hud = add("Loading HUD".into(), W_HUD);
    let brushes = add("Loading menu art".into(), W_BRUSHES);
    let warm = add("Preparing the menus".into(), W_WARM);
    (u, Plan { paks, loadout, models, clips, texture, renderer, char_tex, assemble, hud, brushes, warm, props, foliage })
}

// ------------------------------------------------------------------------------------------------------------------------------------------
// worker side
// ------------------------------------------------------------------------------------------------------------------------------------------
/// Number of worker threads decoding clips.
fn workers() -> usize { std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 8) }

/// The real props to load: the catalog and the library built from it (`Loader::start` does both: they are small), and the kind ids to decode + upload (only the
/// kinds the run will place: the layout's kinds within the radius, or the single test prop; the 49 kinds of the export take ~4 s to upload, the ~20 near the spawn ~1.5 s).
#[derive(Clone)]
pub struct PropsJob { pub cat: Arc<PropCatalog>, pub lib: Arc<crate::props::Library>, pub kinds: Vec<String>, /** POI: the run places the real layout, so the POI around its centre loads too */ pub poi: bool }

struct Job { source: Source, roots: Arc<AssetRoots>, device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat, clip_names: Vec<String>, props: Option<PropsJob> }

/// The resolved `TwoSided` flag of every LOD-0 section's material of the skeletal mesh package `pkg` (`fn_pose::br_character::material_two_sided`: root
/// `UMaterial::TwoSided`, MIC `BasePropertyOverrides` with `bOverride_TwoSided`), in section order = the order of `Part::from_skeletal_mesh`'s primitives.
/// Empty for the exported-tree source (`Source::Dir`: its primitives come from glTF files, no per-section mapping is proven) or when the package cannot be read.
fn mesh_two_sided(source: &Source, paks: Option<&Arc<fn_pak::PakSet>>, pkg: &str) -> Vec<bool> {
    let (Source::Paks(_), Some(set)) = (source, paks) else { return vec![] };
    let src = ClipSourceDir::Pak(set);
    let Ok(p) = fn_pose::loader::package(&src, pkg) else { return vec![] };
    let Ok(mesh) = fn_mesh::SkeletalMesh::parse(&p) else { return vec![] };
    let Some(lod) = mesh.lods.first() else { return vec![] };
    (0..lod.sections.len()).map(|s| mesh.materials.get(mesh.section_material_index(0, s)).and_then(|m| m.material.as_deref())
        .is_some_and(|m| fn_pose::br_character::material_two_sided(&src, m).unwrap_or(false))).collect()
}

fn parts_from(source: &Source, roots: &AssetRoots, paks: Option<&Arc<fn_pak::PakSet>>) -> Result<(CharacterRig, Part), String> {
    match source {
        Source::Dir => {
            let idx = fn_pose::loader::AssetIndex { files: roots.files.clone() };
            let mut parts = fn_pose::loader::old_tree_parts(&idx)?;
            parts.truncate(2); // body, head: the master mesh only donates its IK bones
            let pick = Part::from_gltf_path(idx.find("SK_Pick.gltf")?)?;
            Ok((CharacterRig::new(parts), pick))
        }
        Source::Paks(_) => {
            let set = paks.ok_or("paks not mounted")?;
            let src = ClipSourceDir::Pak(set);
            let mut parts = fn_pose::loader::package_parts(&src)?;
            parts.truncate(2);
            let pkg = fn_pose::loader::package(&src, fn_pose::loader::PICK_PKG)?;
            let mesh = fn_mesh::SkeletalMesh::parse(&pkg).map_err(|e| e.to_string())?;
            Ok((CharacterRig::new(parts), Part::from_skeletal_mesh("SK_Pick", &mesh, 0)?))
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn build_world(job: Job, p: Arc<Progress>, plan: Plan) -> Result<Box<Scene>, String> {
    let Job { source, roots, device, queue, format, clip_names, props: props_job } = job;
    // 1. mount (pak mode)
    let paks = match &source {
        Source::Paks(dir) => {
            let i = plan.paks.ok_or("no pak unit")?; p.begin(i);
            let set = Arc::new(fn_pak::PakSet::open_install(dir).map_err(|e| format!("Cannot open the paks in {}: {e}", dir.display()))?);
            p.end(i); Some(set)
        }
        Source::Dir => None,
    };
    let loadout = if let Some(set) = &paks {
        let i = plan.loadout.ok_or("no starter loadout progress unit")?;
        p.begin(i);
        let mut resources = fn_pose::loadout::Resources::load(&ClipSourceDir::Pak(set))?;
        crate::emote_hook::absorb(set, &mut resources);
        eprintln!("[loadout] loaded {} original item definitions, {} role bindings, {} montages, {} composites, {} sequences", resources.items.len(), resources.bindings.len(), resources.montages.len(), resources.composites.len(), resources.sequences.len());
        eprintln!("[loadout] recovered {} inherited weapon core groups and {} original blendspaces",resources.core_anims.len(),resources.blendspaces.len());
        eprintln!("[loadout] retained {} original gun material packages with full section/parent references",resources.materials.len());
        eprintln!("[loadout] decoded {} original material parameter textures with sampler/colour metadata",resources.material_textures.len());
        eprintln!("[loadout] parsed {} original material shader variants; pass/vertex-factory selection remains unresolved",resources.materials.values().map(|m|m.shaders.len()).sum::<usize>());
        p.end(i);
        Some(resources)
    } else { None };
    // 2. the clip loader: a closure over the source, shared by the pool
    let clip_job: Arc<dyn Fn(&str) -> Result<Loaded, String> + Send + Sync> = match &source {
        Source::Dir => { let files = Arc::new(roots.files.clone()); Arc::new(move |name: &str| fn_legacy::load_clip(&files, &format!("{name}.md5anim")).map(Loaded::Md5)) }
        Source::Paks(_) => {
            let set = paks.clone().ok_or("paks not mounted")?;
            let skel = Arc::new(fn_pose::clip::load_player_skeleton_info(&ClipSourceDir::Pak(&set))?);
            Arc::new(move |name: &str| {
                let mut store = ClipStore::load(&ClipSourceDir::Pak(&set), &[name.to_owned()], &skel, SampleMode::Ue)?;
                store.clips.remove(name).map(|c| Loaded::Fn(Box::new(c))).ok_or_else(|| format!("{name}: decoded no clip"))
            })
        }
    };
    let queue_idx: Arc<Mutex<VecDeque<usize>>> = Arc::new(Mutex::new((0..clip_names.len()).collect()));
    let (tx, rx) = channel::<(usize, Result<Loaded, String>)>();
    let renderer_units = plan.renderer.clone();

    // real props: the kinds to decode (CPU, on a small pool, bounded so only a few kinds' textures are in memory) and upload once the renderer exists
    let prop_kinds: Vec<&fn_propdata::PropKind> = props_job.as_ref().map(|j| crate::props::select_kinds(&j.cat, &j.kinds)).unwrap_or_default();
    let next_prop = std::sync::atomic::AtomicUsize::new(0);
    let (renderer, rig, pick, body_head, real) = std::thread::scope(|s| -> Result<(Renderer, CharacterRig, Part, (fn_render::BuiltModel, fn_render::BuiltModel, fn_render::BuiltModel), Option<crate::props::Real>), String> {
        let prop_rx = if prop_kinds.is_empty() { None } else {
            let (ptx, prx) = std::sync::mpsc::sync_channel::<(usize, Result<fn_render::static_loader::PreparedProp, String>)>(6);
            for _ in 0..workers().min(4) {
                let (ptx, kinds, next) = (ptx.clone(), &prop_kinds, &next_prop);
                s.spawn(move || loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if i >= kinds.len() { break; }
                    let known = std::collections::HashSet::new();
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fn_render::static_loader::prepare_kind(kinds[i], &known))).unwrap_or_else(|_| Err("panicked while decoding".to_owned()));
                    if ptx.send((i, r)).is_err() { break; }
                });
            }
            Some(prx)
        };
        // clip pool
        for _ in 0..workers() {
            let (queue_idx, tx, job, p, names, units) = (queue_idx.clone(), tx.clone(), clip_job.clone(), p.clone(), &clip_names, &plan.clips);
            s.spawn(move || loop {
                let Some(i) = queue_idx.lock().ok().and_then(|mut q| q.pop_front()) else { break };
                p.begin(units[i]);
                let r = job(&names[i]);
                p.end(units[i]);
                if tx.send((i, r)).is_err() { break; }
            });
        }
        drop(tx);
        // renderer thread: ground texture, then every renderer phase (each announced before it runs)
        let rh = {
            let (p, roots, device, queue, tex_unit) = (p.clone(), roots.clone(), device.clone(), queue.clone(), plan.texture);
            s.spawn(move || -> Result<Renderer, String> {
                p.begin(tex_unit);
                let ground = roots.load_rgba("T_Grasslands_D.png")?;
                p.end(tex_unit);
                let sky_dir = roots.find("dome_optimized.bin").and_then(|f| f.parent()).ok_or("Missing asset: dome_optimized.bin")?;
                let sky = SkyAssets::load(sky_dir)?;
                let mut current: Option<usize> = None;
                let r = Renderer::new_with_progress(&device, &queue, format, &RendererAssets { sky, ground_texture: ground, ground_mips: None }, &mut |label| {
                    if let Some(c) = current.take() { p.end(c); }
                    if let Some(i) = renderer_units.iter().copied().find(|&i| p.with(|c, _| c.units[i].label == label)) { p.begin(i); current = Some(i); }
                })?;
                if let Some(c) = current.take() { p.end(c); }
                Ok(r)
            })
        };
        // this thread: models, then the characters' textures (disk decode)
        p.begin(plan.models);
        let (rig, pick) = parts_from(&source, &roots, paks.as_ref())?;
        p.end(plan.models);
        p.begin(plan.char_tex);
        // BR CHARACTER HOOK: the real Athena body + head from the packages replace the current body / head (same master pose, same two slots); None = keep the current ones
        let br = crate::br_character::load_for(&source, paks.as_ref());
        let (rig, body, head) = match br {
            Some(m) => (fn_pose::CharacterRig { parts: m.rig_parts, ..rig }, m.body, m.head),
            None => {
                let b = part_primitives(&rig.parts[0], &roots, MaterialKind::Character, &mesh_two_sided(&source, paks.as_ref(), fn_pose::loader::BODY_PKG));
                let h = part_primitives(&rig.parts[1], &roots, MaterialKind::Character, &mesh_two_sided(&source, paks.as_ref(), fn_pose::loader::HEAD_PKG));
                (rig, b, h)
            }
        };
        let mut pickp = part_primitives(&pick, &roots, MaterialKind::Prop, &mesh_two_sided(&source, paks.as_ref(), fn_pose::loader::PICK_PKG));
        make_rigid(&mut pickp);
        p.end(plan.char_tex);
        let lib = props_job.as_ref().map(|j| j.lib.clone());
        let mut renderer = rh.join().map_err(|_| "the renderer thread panicked".to_owned())??;
        // LOAD TIME (2026-10-06): the body / head / pickaxe GPU models (texture uploads with their CPU mip chains, ~2.8 s) build on their own threads while
        // this thread uploads the props, instead of one after another at the end of the load (`Scene::with_models`; the same upload, only earlier)
        p.begin(plan.assemble);
        let models = {
            let palette = |i: usize| rig.parts.get(i).map(|p| p.palette_len()).unwrap_or(1);
            let (pb, ph, mb) = (palette(0), palette(1), renderer.model_builder());
            let (bb, hb) = (mb.clone(), mb.clone());
            (s.spawn(move || bb.build(&body, pb, "model body")), s.spawn(move || hb.build(&head, ph, "model head")), s.spawn(move || mb.build(&pickp, 1, "model pickaxe")))
        };
        // upload the decoded kinds (one progress unit each); a kind that fails stays a procedural placeholder, nothing here can fail the load
        let real = match (lib, prop_rx) {
            (Some(lib), Some(prx)) => {
                let mut cache = HashMap::new();
                let mut meshes: Vec<Option<fn_render::StaticMeshHandle>> = vec![None; lib.protos.len()];
                let mut skipped = vec![];
                for _ in 0..prop_kinds.len() {
                    let Ok((i, r)) = prx.recv() else { break };
                    let unit = plan.props.get(i).copied();
                    if let Some(u) = unit { p.begin(u); }
                    match r {
                        Ok(prep) => { let id = prep.id.clone(); let lp = fn_render::static_loader::upload_prepared(&mut renderer, prep, &mut cache); if let Some(pi) = lib.proto_index(&id) { meshes[pi as usize] = Some(lp.mesh); } }
                        Err(e) => skipped.push((prop_kinds[i].id.clone(), e)),
                    }
                    if let Some(u) = unit { p.end(u); }
                }
                for &u in &plan.props { p.end(u); }
                Some(crate::props::Real { lib, meshes, skipped })
            }
            _ => { for &u in &plan.props { p.end(u); } None }
        };
        let panicked = |what: &str| format!("the {what} model upload panicked");
        let models = (models.0.join().map_err(|_| panicked("body"))?, models.1.join().map_err(|_| panicked("head"))?, models.2.join().map_err(|_| panicked("pickaxe"))?);
        Ok((renderer, rig, pick, models, real))
    })?;
    let _ = pick;
    // FOLIAGE HOOK: the ambient ground cover (one progress unit), uploaded through the same static-mesh path as the props; a failure only logs and leaves the world without grass
    let mut renderer = renderer;
    let grass = match (plan.foliage, props_job.as_ref()) {
        (Some(u), Some(j)) => { p.begin(u); let g = crate::foliage::load(&mut renderer, &j.cat.dir, &j.lib); p.end(u); g }
        _ => None,
    };
    // BACKDROP HOOK: the real landscape / ocean / far skybox beyond the plate (optional export `props/backdrop`); a failure only logs and keeps the old flat ground ring
    let mut ground_ref_cm = None; // POI (dev): the plate's UE height (the backdrop's ground reference)
    if let Some(dir) = crate::backdrop_load::find_dir(&roots.roots) {
        if let Ok(c) = crate::backdrop_load::read_centre(&dir) { crate::storm_hook::set_mapping(c, crate::props::ANCHOR); } // STORM HOOK: UE level cm -> baseplate mapping of the storm
        match crate::backdrop_load::read_centre(&dir).and_then(|c| crate::backdrop_load::load(&dir, c, crate::props::ANCHOR, fn_game::HALF)) { Ok(d) => { crate::storm_hook::set_ground_ref(d.ground_ref_cm); /* STORM HOOK */ ground_ref_cm = Some(d.ground_ref_cm); renderer.set_backdrop(&d) } Err(e) => eprintln!("[backdrop] {e}: no landscape / ocean") }
    }
    // POI: the town around the layout centre (`<props>/poi/*.json`: Lonely Lodge at the default centre, `--world-center` moves it): every static-mesh
    // component drawn by statics_render, the actors' collision / building data handed to the props (`Library::populate`)
    if let Some(j) = props_job.as_ref().filter(|j| j.poi) {
        let c = crate::poi::world_center().unwrap_or(j.lib.center_cm);
        crate::poi::load(&j.cat.dir, c, if c[2] != 0. { c[2] } else { ground_ref_cm.unwrap_or(0.) });
    }
    // 3. collect the clips (all workers are done once the scope ended: the channel holds every result)
    let total = clip_names.len();
    let mut md5: HashMap<String, Md5Clip> = HashMap::new();
    let mut fnclips = ClipStore::new();
    let mut got = 0;
    while got < total {
        match rx.recv() {
            Ok((i, r)) => {
                let name = &clip_names[i];
                match r.map_err(|e| format!("Cannot load animation {name}: {e}"))? { Loaded::Md5(c) => { md5.insert(name.clone(), c); } Loaded::Fn(c) => { fnclips.insert(name, *c); } }
                got += 1;
            }
            Err(_) => return Err("the animation workers stopped unexpectedly".into()),
        }
    }
    if let Some(resources)=&loadout {
        let added=resources.install_player_clips(&mut fnclips);
        eprintln!("[loadout] installed {added} additional original player clips; weapon-skeleton sequences remain separate");
    }
    // 4. install the models (uploaded above, `plan.assemble` started there)
    let (body, head, pickp) = body_head;
    crate::br_character::take_female_clips(&mut fnclips);
    let clips: Box<dyn ClipLibrary + Send> = match source { Source::Dir => Box::new(md5), Source::Paks(_) => Box::new(fnclips) };
    let mut scene = Scene::with_models(renderer, Character::new(rig, clips), body, head, pickp);
    scene.character.part_anim = crate::part_anim_glue::take(); // PART ANIM HOOK
    scene.real = real;
    scene.grass = grass; // FOLIAGE HOOK
    if let Some(resources)=&loadout {
        let hand=fn_pose::SocketDef::from_original_yaw_socket(0,&resources.player_hand_socket)?;
        if scene.character.rig.parts[0].tree.bone_index(&hand.bone).is_none(){return Err("original hand socket bone missing from character".into());}
        // Pak mode uses the actual serialized socket, not the legacy default.
        // Scene evaluates this socket twice: once on the displayed post-process
        // pose and once immediately before CopyBone / hand IK for the weapon.
        scene.character.rig.sockets=vec![hand];
        eprintln!("[loadout] installed original player RightHand socket for final-pose weapon attachment");
    }
    scene.loadout = loadout;
    p.end(plan.assemble);
    Ok(Box::new(scene))
}

// BROWSER PORT: the same load with every GPU step on the main thread (wgpu objects cannot leave it on the web): tools/web/fn-web/src/loader_web.rs
#[cfg(target_arch = "wasm32")]
#[path = "../../../tools/web/fn-web/src/loader_web.rs"]
mod web;

// ------------------------------------------------------------------------------------------------------------------------------------------
// UI-thread side
// ------------------------------------------------------------------------------------------------------------------------------------------
/// Command ranges of at most `max` commands that never split a clip scope (so each range renders on its own).
pub fn slice_ranges(list: &RenderList, max: usize) -> Vec<(usize, usize)> {
    let (mut out, mut start, mut depth) = (vec![], 0usize, 0i32);
    for (i, c) in list.cmds.iter().enumerate() {
        match c { Cmd::PushClip(_) => depth += 1, Cmd::PopClip => depth -= 1, _ => {} }
        if depth <= 0 && i + 1 - start >= max { out.push((start, i + 1)); start = i + 1; }
    }
    if start < list.cmds.len() { out.push((start, list.cmds.len())); }
    out
}

/// Commands per warm-up render slice: small enough that a slice which has to build a pipeline stays short.
const WARM_SLICE_CMDS: usize = 24;
const WARM_PASSES: usize = 3;
const WARM_SIZE: (u32, u32) = (1280, 720);
/// Brush textures decoded per poll at most (also bounded by the time budget).
const BRUSH_CHUNK: usize = 6;

enum Ui {
    Hud,
    Brushes { files: Vec<String>, i: usize },
    WarmBuildHud { pass: usize },
    WarmBuildMenu { pass: usize },
    WarmRender { pass: usize, ranges: Vec<(usize, usize)>, at: usize },
    WarmFinish,
    Done,
}

pub enum State { Running, Done(Box<Scene>) }

pub struct Loader {
    progress: Arc<Progress>,
    plan: Plan,
    result: Receiver<Result<Box<Scene>, String>>,
    scene: Option<Box<Scene>>,
    ui: Ui,
    /// The next UI-thread unit was announced (its label has been drawn once) but has not run yet.
    announced: bool,
    target: Option<fn_render::OffscreenTarget>,
    /// The warm-up pass being built / rendered (the UI's own list is rebuilt every frame by the screen that is showing, so the loader keeps its own).
    warm_list: RenderList,
    host: fn_ui::menu::pages::NullHost,
    pub started: Instant,
    /// Longest single UI-thread slice (ms) and the number of slices over 50 ms.
    pub max_slice_ms: f32,
    pub slices_over_50ms: u32,
    /// UI-thread operations that took more than 15 ms: (what, ms).
    pub slow_ops: Vec<(String, f32)>,
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    /// BROWSER PORT: the main-thread half of the world load (renderer + uploads), driven by Loader::poll
    #[cfg(target_arch = "wasm32")]
    world: Option<web::WorldBuild>,
}

impl Loader {
    /// Spawns the worker side. `device` / `queue` are clones of the window's (the renderer is built on a worker thread).
    pub fn start(source: Source, roots: Arc<AssetRoots>, device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat, props: Option<PropsJob>) -> Loader {
        let clip_names = fn_legacy::prototype_clip_names();
        let prop_ids: Vec<String> = props.as_ref().map(|j| crate::props::select_kinds(&j.cat, &j.kinds).iter().map(|k| k.id.clone()).collect()).unwrap_or_default();
        // FOLIAGE HOOK: one extra unit when the ground cover export exists next to the props
        let foliage_label = props.as_ref().and_then(|j| crate::foliage::Spec::read(&j.cat.dir).ok()).map(|s| format!("Loading ground cover: {}", s.name));
        let (units, plan_) = plan_with_foliage(&source, &clip_names, &prop_ids, foliage_label);
        let progress = Progress::new(units);
        let (tx, rx) = channel();
        let job = Job { source, roots, device: device.clone(), queue, format, clip_names, props };
        let (p2, plan2) = (progress.clone(), plan_.clone());
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::Builder::new().name("load-world".into()).spawn(move || { let r = build_world(job, p2, plan2); let _ = tx.send(r); }).ok();
        #[cfg(target_arch = "wasm32")]
        let world = Some(web::WorldBuild::start(job, p2, plan2, tx));
        Loader { progress, plan: plan_, result: rx, scene: None, ui: Ui::Hud, announced: false, target: None, warm_list: RenderList::new(), host: fn_ui::menu::pages::NullHost::new(), started: Instant::now(), max_slice_ms: 0., slices_over_50ms: 0, slow_ops: vec![], device, format, #[cfg(target_arch = "wasm32")] world }
    }
    pub fn fraction(&self) -> f32 { self.progress.fraction() }
    pub fn label(&self) -> String { self.progress.label() }
    pub fn progress(&self) -> &Arc<Progress> { &self.progress }
    /// FRONT END: the login screen's share of the load = the worker-thread units (paks, starter loadout, models, animation clips, ground texture); the
    /// renderer, the uploads and the UI warm-up follow behind the level-travel loading screen.
    pub fn login_share_done(&self) -> bool {
        let p = &self.plan;
        p.paks.into_iter().chain(p.loadout).chain([p.models, p.texture]).chain(p.clips.iter().copied()).all(|i| self.progress.is_finished(i))
    }
    pub fn finished(&self) -> bool { self.scene.is_some() && matches!(self.ui, Ui::Done) }
    /// "seconds  label  (start offset)" per finished unit, the longest UI-thread slice and the total.
    pub fn timings_text(&self) -> String {
        let mut s: String = self.progress.timings().iter().map(|(l, secs, start)| format!("{secs:8.3} s  {l}  (started at {start:.3} s)\n")).collect();
        s.push_str(&format!("{:8.3} s  total   longest UI-thread slice {:.1} ms, slices over 50 ms: {}\n", self.started.elapsed().as_secs_f32(), self.max_slice_ms, self.slices_over_50ms));
        for (what, ms) in &self.slow_ops { s.push_str(&format!("          UI-thread op over 15 ms: {what}: {ms:.1} ms\n")); }
        s
    }

    /// One frame's worth of UI-thread work (<= `budget` plus at most one slice) and the poll of the worker. `Done` once everything finished.
    pub fn poll(&mut self, ui: &mut UiLayer, budget: Duration) -> Result<State, String> {
        #[cfg(target_arch = "wasm32")]
        if let Some(w) = self.world.as_mut() { w.step(budget); }
        match self.result.try_recv() {
            Ok(Ok(scene)) => self.scene = Some(scene),
            Ok(Err(e)) => return Err(e),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => if self.scene.is_none() { return Err("the loading thread stopped unexpectedly".into()) },
        }
        let t0 = Instant::now();
        while !matches!(self.ui, Ui::Done) {
            // announce a unit's first operation one frame before it runs, so the screen already says what is being done
            let unit = match &self.ui { Ui::Hud => Some(self.plan.hud), Ui::Brushes { i: 0, .. } => Some(self.plan.brushes), Ui::WarmBuildHud { pass: 0 } => Some(self.plan.warm), _ => None };
            if let Some(u) = unit { if !self.announced { self.progress.begin(u); self.announced = true; return Ok(self.state_now()); } }
            self.announced = false;
            let slice = Instant::now();
            let what = self.ui_op_name();
            self.step_ui(ui);
            let ms = slice.elapsed().as_secs_f32() * 1000.;
            if ms > 15. { self.slow_ops.push((what, ms)); }
            self.max_slice_ms = self.max_slice_ms.max(ms);
            if ms > 50. { self.slices_over_50ms += 1; }
            if t0.elapsed() >= budget { break; }
        }
        Ok(self.state_now())
    }
    fn ui_op_name(&self) -> String {
        match &self.ui { Ui::Hud => "HUD scratch present".into(), Ui::Brushes { i, files } => format!("menu brushes {i}..{}", (*i + BRUSH_CHUNK).min(files.len())), Ui::WarmBuildHud { pass } => format!("warm pass {pass}: build HUD"), Ui::WarmBuildMenu { pass } => format!("warm pass {pass}: build menu"),
            Ui::WarmRender { pass, ranges, at } => format!("warm pass {pass}: render slice {at}/{}", ranges.len()), Ui::WarmFinish => "warm finish".into(), Ui::Done => "done".into() }
    }
    fn state_now(&mut self) -> State {
        if self.scene.is_some() && matches!(self.ui, Ui::Done) { State::Done(self.scene.take().expect("checked")) } else { State::Running }
    }

    fn step_ui(&mut self, ui: &mut UiLayer) {
        let (w, h) = (WARM_SIZE.0 as f32, WARM_SIZE.1 as f32);
        let ui_state = std::mem::replace(&mut self.ui, Ui::Done);
        self.ui = match ui_state {
            Ui::Hud => {
                // build a HUD at the reference size into a scratch list: loads the HUD art and builds its glyph atlases
                let mut scratch = RenderList::new();
                ui.hud.set_state(&fn_ui::screens::hud::HudState::default());
                ui.hud.present(&mut ui.ctx, 1920., 1080., &mut scratch);
                fn_ui::screens::game_phase::prewarm(&mut ui.ctx);
                self.progress.end(self.plan.hud);
                Ui::Brushes { files: fn_ui::menu::lower::brush_files().into_iter().collect(), i: 0 }
            }
            Ui::Brushes { files, mut i } => {
                let end = (i + BRUSH_CHUNK).min(files.len());
                while i < end { ui.ctx.assets.texture(&files[i]); i += 1; }
                if i >= files.len() { self.progress.end(self.plan.brushes); Ui::WarmBuildHud { pass: 0 } } else { Ui::Brushes { files, i } }
            }
            Ui::WarmBuildHud { pass } => {
                if pass == 0 {
                    self.target = Some(fn_render::OffscreenTarget::new(&self.device, WARM_SIZE.0, WARM_SIZE.1, self.format));
                    ui.menu.open();
                    ui.menu.push(Box::new(fn_ui::menu::pause::SettingsScreen::new()));
                }
                self.warm_list.clear();
                ui.hud.set_state(&fn_ui::screens::hud::HudState::default());
                ui.hud.present(&mut ui.ctx, w, h, &mut self.warm_list);
                Ui::WarmBuildMenu { pass }
            }
            Ui::WarmBuildMenu { pass } => {
                let draw = ui.menu.frame_with(&mut self.host.cx(), w, h, 0.016);
                fn_ui::menu::lower::lower(draw, &mut ui.ctx, w, h, &mut self.warm_list);
                let ranges = slice_ranges(&self.warm_list, WARM_SLICE_CMDS);
                Ui::WarmRender { pass, ranges, at: 0 }
            }
            Ui::WarmRender { pass, ranges, at } => {
                if let (Some(&(a, b)), Some(target)) = (ranges.get(at), self.target.as_ref()) {
                    let sub = RenderList { cmds: self.warm_list.cmds[a..b].to_vec() };
                    ui.renderer.render(&sub, &mut ui.ctx.assets, &target.view, WARM_SIZE.0, WARM_SIZE.1, if at == 0 { Some([0., 0., 0., 1.]) } else { None });
                }
                if at + 1 < ranges.len() { Ui::WarmRender { pass, ranges, at: at + 1 } }
                else if pass + 1 < WARM_PASSES { Ui::WarmBuildHud { pass: pass + 1 } } else { Ui::WarmFinish }
            }
            Ui::WarmFinish => {
                ui.menu.close();
                ui.menu.set_visibility(0.);
                let _ = ui.menu.take_requests();
                ui.begin();
                self.warm_list = RenderList::new();
                self.target = None;
                self.progress.end(self.plan.warm);
                Ui::Done
            }
            Ui::Done => Ui::Done,
        };
    }
}

// ------------------------------------------------------------------------------------------------------------------------------------------
// COSMETIC SWAP (run-time skin / harvesting tool changes)
// ------------------------------------------------------------------------------------------------------------------------------------------
/// The front-end half of `fn_game::cosmetic_swap`: the sim only RECORDS the selection (`GameSim::cosmetics`, `set_skin` / `set_pickaxe` / `set_glider`; fn-game cannot depend on
/// fn-pose); [`cosmetic::Runtime::tick`], called once per frame BEFORE `Scene::update`, takes the pending kinds and rebuilds what changed:
///  * skin: `BrCharacter::load(paks, Item.hero)` (the part list is built from scratch from the hero's specializations: body / head / hat / face accessories / material overrides, so nothing
///    of the previous skin survives) + the parts' AnimBP runtime + the GPU upload of the new body / head models, ALL on a background thread (~0.4 s); when ready, the render thread swaps
///    rig parts / part AnimBP / the two GPU models in one step (`Scene::install_character`: pose, sockets, held weapon, position, camera and animation state are untouched).
///  * pickaxe: `loadout_melee::load_for(paks, Item.package)` (WID, actor chain, equip / swing montages) + the WID's mesh decoded with its own material textures + GPU upload (background), then
///    `Scene::install_pickaxe` replaces the held mesh.
///  * glider / back bling: only resolved and logged (the 1.8 port has no skydive / glide state or glider rendering to wire them into; 1.8 ships no back bling item class).
/// A newer request of the same kind supersedes a job still running (its result is dropped).
pub mod cosmetic {
    use super::*;
    use fn_game::cosmetic_swap::{Item, Kind};
    use fn_game::GameSim;
    use fn_pose::br_character::{decode_texture, effective_texture_params, RgbaTexture};
    use fn_pose::loadout_melee::PickaxeSet;
    use fn_render::{BuiltModel, MaterialImages, ModelBuilder, SkinnedPrimitive, SkinnedVertex};

    /// CPU result of a skin decode: everything the pawn is made of.
    pub struct SkinDecoded {
        pub item: Item,
        pub hero: String,
        /// Package of every part of the hero, in draw order (body, head, then hats / face accessories ...).
        pub parts: Vec<String>,
        /// `[body, head]` skeletons (master IK bones appended) + the primitives (emptied once uploaded) + the load report.
        pub model: crate::br_character::BrModel,
        pub female_clips: Option<Arc<fn_pose::ClipStore>>,
        pub part_anim: Option<fn_pose::part_anim::PartAnimRig>,
        /// CPU decode time, and the GPU upload time (worker thread) added by the job.
        pub ms: u128,
        pub upload_ms: u128,
    }
    /// A decoded skin plus its GPU models (`None` when the target has no GPU, i.e. a headless test).
    pub struct SkinReady { pub decoded: SkinDecoded, pub gpu: Option<(BuiltModel, BuiltModel)> }

    /// CPU result of a pickaxe decode.
    pub struct PickDecoded {
        pub item: Item,
        pub set: PickaxeSet,
        /// Package of the held mesh (`WID.WeaponMeshOverride`).
        pub mesh: String,
        /// Rigid (joint 0) primitives with the mesh's own material textures (emptied once uploaded).
        pub prims: Vec<SkinnedPrimitive>,
        pub notes: Vec<String>,
        pub ms: u128,
        pub upload_ms: u128,
    }
    pub struct PickReady { pub decoded: PickDecoded, pub gpu: Option<BuiltModel> }

    /// What the swap installs into (the [`Scene`]; a recording mock in the headless tests). The `install_*` calls take the GPU models / runtimes out of the `Ready` value.
    pub trait Target {
        /// GPU upload handle for the worker threads (`None`: no GPU, only the CPU result is delivered).
        fn builder(&self) -> Option<ModelBuilder>;
        fn install_skin(&mut self, r: &mut SkinReady) -> Result<String, String>;
        fn install_pickaxe(&mut self, r: &mut PickReady) -> Result<String, String>;
    }
    impl Target for Scene {
        fn builder(&self) -> Option<ModelBuilder> { Some(self.model_builder()) }
        fn install_skin(&mut self, r: &mut SkinReady) -> Result<String, String> {
            let d = &mut r.decoded;
            // the hand socket the weapon is attached at must exist on the new skeleton (the loader checks the same at start-up)
            for s in &self.character.rig.sockets {
                if d.model.rig_parts.get(s.part).and_then(|p| p.tree.bone_index(&s.bone)).is_none() { return Err(format!("{}: socket bone {} missing from the new character; keeping the current one", d.hero, s.bone)); }
            }
            let (body, head) = r.gpu.take().ok_or("no GPU models were uploaded")?;
            self.install_character(std::mem::take(&mut d.model.rig_parts), d.part_anim.take(), body, head);
            if let Some(f) = d.female_clips.take() {
                if self.character.clips.clip("Pose_Melee_TwoHanded_CMF").is_none() {
                    let base = std::mem::replace(&mut self.character.clips, Box::new(ClipStore::new()));
                    self.character.clips = Box::new(crate::weapon_runtime::LayeredClips { base, extra: ClipStore { clips: f.clips.clone() } });
                }
            }
            Ok(format!("{} ({}): {} part(s) {:?}, {} ms decode + {} ms GPU upload, both off-thread", d.item.display_name, d.hero, d.parts.len(), d.parts, d.ms, d.upload_ms))
        }
        fn install_pickaxe(&mut self, r: &mut PickReady) -> Result<String, String> {
            let m = r.gpu.take().ok_or("no GPU model was uploaded")?;
            Scene::install_pickaxe(self, m);
            self.pick_mesh = r.decoded.mesh.clone(); // HELD PICKAXE LIGHTING: the original-material draw follows the swap
            Ok(format!("{} -> {} mesh {} ({} ms decode + {} ms GPU upload, both off-thread)", r.decoded.item.display_name, r.decoded.set.weapon_definition, r.decoded.mesh, r.decoded.ms, r.decoded.upload_ms))
        }
    }

    fn rgba(t: &Option<RgbaTexture>) -> Option<image::RgbaImage> { let t = t.as_ref()?; image::RgbaImage::from_raw(t.width as u32, t.height as u32, t.rgba.clone()) }

    /// CPU half of a skin swap (any thread): the hero's parts, materials and textures, and the parts' AnimBP runtime.
    pub fn decode_skin(set: &Arc<fn_pak::PakSet>, item: &Item) -> Result<SkinDecoded, String> {
        let t = Instant::now();
        let hero = item.hero.clone().ok_or_else(|| format!("{} has no HeroDefinition", item.id))?;
        let src = ClipSourceDir::Pak(set);
        let ch = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fn_pose::br_character::with_cooked_only(|| fn_pose::br_character::BrCharacter::load(&src, &hero)))).map_err(|_| format!("{hero}: the character loader panicked"))??;
        let part_anim = crate::part_anim_glue::build(&ch, &src);
        let parts = ch.parts.iter().map(|p| p.def.package.clone()).collect();
        let model = crate::br_character::from_character(&ch);
        let female_clips = if ch.female { Some(crate::br_character::female_clip_store(set)?) } else { None };
        Ok(SkinDecoded { item: item.clone(), hero, parts, model, part_anim, female_clips, ms: t.elapsed().as_millis(), upload_ms: 0 })
    }

    /// CPU half of a pickaxe swap (any thread). The held mesh is the WID's `WeaponMeshOverride`; its textures come from the mesh's own materials (the parameters the BR character
    /// materials use: `Diffuse`, `Normals`, `SpecularMasks`, `M`). Attached rigidly like the default pickaxe (`make_rigid`).
    pub fn decode_pickaxe(set: &Arc<fn_pak::PakSet>, item: &Item) -> Result<PickDecoded, String> {
        let t = Instant::now();
        let src = ClipSourceDir::Pak(set);
        let ps = fn_pose::loadout_melee::load_for(&src, &item.package)?;
        if !ps.stw_paths().is_empty() { return Err(format!("{}: resolved a Save the World path {:?}", item.id, ps.stw_paths())); }
        let pkg = fn_pose::loader::package(&src, &ps.weapon_mesh)?;
        let mesh = fn_mesh::SkeletalMesh::parse(&pkg).map_err(|e| format!("{}: {e}", ps.weapon_mesh))?;
        let name = ps.weapon_mesh.rsplit('/').next().unwrap_or("pickaxe");
        let part = Part::from_skeletal_mesh(name, &mesh, 0)?;
        if part.prims.len() != mesh.lods[0].sections.len() { return Err(format!("{}: {} primitives for {} sections", ps.weapon_mesh, part.prims.len(), mesh.lods[0].sections.len())); }
        let mut notes = vec![];
        let mut cache: HashMap<String, Option<RgbaTexture>> = HashMap::new();
        let mut prims = vec![];
        for (s, p) in part.prims.iter().enumerate() {
            let slot = mesh.section_material_index(0, s);
            let mat = mesh.materials.get(slot).and_then(|m| m.material.clone());
            let (mut diffuse, mut normal, mut spec, mut mask) = (None, None, None, None);
            match &mat {
                None => notes.push(format!("section {s}: no material")),
                Some(m) => match effective_texture_params(&src, m) {
                    Err(e) => notes.push(format!("section {s}: {e}")),
                    Ok(chain) => {
                        let mut get = |param: &str| -> Option<RgbaTexture> {
                            let path = chain.textures.get(param)?;
                            cache.entry(path.clone()).or_insert_with(|| decode_texture(&src, path).ok()).clone()
                        };
                        diffuse = get("Diffuse"); normal = get("Normals"); spec = get("SpecularMasks"); mask = get("M");
                        if diffuse.is_none() { let mut k: Vec<&String> = chain.textures.keys().collect(); k.sort(); notes.push(format!("section {s}: {m} has no Diffuse texture parameter (parameters: {k:?})")); }
                    }
                },
            }
            let vertices = (0..p.positions.len()).map(|i| SkinnedVertex { pos: p.positions[i], normal: p.normals[i], uv: p.uvs[i], joints: p.joints[i], weights: p.weights[i] }).collect();
            let two_sided = mat.as_deref().is_some_and(|m| fn_pose::br_character::material_two_sided(&src, m).unwrap_or(false)); // shadow-pass culling (UE4.16 ShadowDepthRendering.cpp:1059)
            prims.push(SkinnedPrimitive { vertices, indices: p.indices.clone(), material: MaterialImages { diffuse: rgba(&diffuse), normal: rgba(&normal), spec: rgba(&spec), mask: rgba(&mask), ue: None, two_sided, cooked: [&diffuse, &normal, &spec, &mask].map(|t| t.as_ref().and_then(crate::br_character::cooked_of)) }, kind: MaterialKind::Prop });
        }
        make_rigid(&mut prims);
        Ok(PickDecoded { item: item.clone(), mesh: ps.weapon_mesh.clone(), set: ps, prims, notes, ms: t.elapsed().as_millis(), upload_ms: 0 })
    }

    struct Job<T> { rx: Receiver<Result<T, String>>, id: String }
    fn spawn<T: Send + 'static>(name: &str, id: &str, f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Job<T> {
        let (tx, rx) = channel();
        if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(move || { let _ = tx.send(f()); }) {
            let (tx2, rx2) = channel();
            let _ = tx2.send(Err(format!("cannot start the {name} thread: {e}")));
            return Job { rx: rx2, id: id.to_owned() };
        }
        Job { rx, id: id.to_owned() }
    }

    /// The runtime state: the jobs in flight and what is currently installed.
    #[derive(Default)]
    pub struct Runtime {
        #[cfg(not(target_arch = "wasm32"))]
        skin: Option<Job<SkinReady>>,
        #[cfg(not(target_arch = "wasm32"))]
        pick: Option<Job<PickReady>>,
        /// BROWSER PORT: the worker delivers the CPU half only; the GPU upload runs on the main thread when it arrives (wgpu objects cannot leave it)
        #[cfg(target_arch = "wasm32")]
        skin: Option<Job<SkinDecoded>>,
        #[cfg(target_arch = "wasm32")]
        pick: Option<Job<PickDecoded>>,
        /// Package list of the installed skin's parts (what the pawn is made of now) and its item id.
        pub installed_parts: Vec<String>,
        pub installed_skin: Option<String>,
        /// The resolved data (WID, actor chain, equip montage, swing montage + variants) of the installed harvesting tool: what the weapon / inventory layer reads.
        pub pickaxe: Option<PickaxeSet>,
        pub installed_pickaxe: Option<String>,
        /// The held mesh package of the installed pickaxe.
        pub pickaxe_mesh: Option<String>,
        pub installs: u32,
    }
    impl Runtime {
        pub fn busy(&self) -> bool { self.skin.is_some() || self.pick.is_some() }

        /// One frame: takes the sim's pending cosmetic changes, starts a background rebuild for each, and installs the ones that finished. Returns the log lines (also printed).
        pub fn tick<T: Target>(&mut self, sim: &mut GameSim, target: &mut T) -> Vec<String> {
            let mut log = vec![];
            for kind in sim.cosmetics.take_pending() {
                let Some(item) = sim.cosmetics.get(kind).cloned() else { continue };
                match kind {
                    Kind::Skin => {
                        let b = target.builder();
                        log.push(format!("[cosmetic] skin {}: decoding on a background thread", item.id));
                        #[cfg(not(target_arch = "wasm32"))]
                        let job = spawn("cosmetic-skin", &item.id.clone(), move || {
                            let set = fn_game::cosmetic_swap::paks()?;
                            let mut decoded = decode_skin(&set, &item)?;
                            let tu = Instant::now();
                            let gpu = b.map(|b| {
                                let m = &decoded.model;
                                let pal = |i: usize| m.rig_parts.get(i).map(|p| p.palette_len()).unwrap_or(1);
                                (b.build(&m.body, pal(0), "swap body"), b.build(&m.head, pal(1), "swap head"))
                            });
                            decoded.upload_ms = tu.elapsed().as_millis();
                            if gpu.is_some() { decoded.model.body = vec![]; decoded.model.head = vec![]; } // uploaded: free the CPU copies
                            Ok(SkinReady { decoded, gpu })
                        });
                        #[cfg(target_arch = "wasm32")]
                        let job = { drop(b); spawn("cosmetic-skin", &item.id.clone(), move || { let set = fn_game::cosmetic_swap::paks()?; decode_skin(&set, &item) }) };
                        self.skin = Some(job);
                    }
                    Kind::Pickaxe => {
                        let b = target.builder();
                        log.push(format!("[cosmetic] pickaxe {}: decoding on a background thread", item.id));
                        #[cfg(not(target_arch = "wasm32"))]
                        let job = spawn("cosmetic-pickaxe", &item.id.clone(), move || {
                            let set = fn_game::cosmetic_swap::paks()?;
                            let mut decoded = decode_pickaxe(&set, &item)?;
                            let tu = Instant::now();
                            let gpu = b.map(|b| b.build(&decoded.prims, 1, "swap pickaxe"));
                            decoded.upload_ms = tu.elapsed().as_millis();
                            if gpu.is_some() { decoded.prims = vec![]; }
                            Ok(PickReady { decoded, gpu })
                        });
                        #[cfg(target_arch = "wasm32")]
                        let job = { drop(b); spawn("cosmetic-pickaxe", &item.id.clone(), move || { let set = fn_game::cosmetic_swap::paks()?; decode_pickaxe(&set, &item) }) };
                        self.pick = Some(job);
                    }
                    Kind::Glider => log.push(format!("[cosmetic] glider {} selected ({:?}): the 1.8 port has no skydive / glide state and draws no glider, nothing to rebuild", item.id, item.assets)),
                    Kind::Backbling => log.push(format!("[cosmetic] back bling {} selected: not drawn (1.8 ships no AthenaBackpackItemDefinition)", item.id)),
                }
            }
            if let Some(job) = self.skin.take() {
                #[cfg(target_arch = "wasm32")]
                let polled = job.rx.try_recv().map(|r| r.map(|d| web_upload_skin(d, &*target)));
                #[cfg(not(target_arch = "wasm32"))]
                let polled = job.rx.try_recv();
                match polled {
                    Ok(Ok(mut ready)) => match target.install_skin(&mut ready) {
                        Ok(m) => { log.push(format!("[cosmetic] skin installed: {m}")); self.installed_parts = ready.decoded.parts.clone(); self.installed_skin = Some(job.id.clone()); self.installs += 1; }
                        Err(e) => log.push(format!("[cosmetic] skin {} ({}) NOT installed: {e}", job.id, ready.decoded.hero)),
                    },
                    Ok(Err(e)) => log.push(format!("[cosmetic] skin {} cannot load: {e}; the current character stays", job.id)),
                    Err(TryRecvError::Empty) => self.skin = Some(job),
                    Err(TryRecvError::Disconnected) => log.push(format!("[cosmetic] skin {}: the loading thread stopped unexpectedly (the current character stays)", job.id)),
                }
            }
            if let Some(job) = self.pick.take() {
                #[cfg(target_arch = "wasm32")]
                let polled = job.rx.try_recv().map(|r| r.map(|d| web_upload_pickaxe(d, &*target)));
                #[cfg(not(target_arch = "wasm32"))]
                let polled = job.rx.try_recv();
                match polled {
                    Ok(Ok(mut ready)) => {
                        for n in &ready.decoded.notes { log.push(format!("[cosmetic] pickaxe {}: {n}", job.id)); }
                        match target.install_pickaxe(&mut ready) {
                            Ok(m) => { log.push(format!("[cosmetic] pickaxe installed: {m}")); self.pickaxe_mesh = Some(ready.decoded.mesh.clone()); self.pickaxe = Some(ready.decoded.set); self.installed_pickaxe = Some(job.id.clone()); self.installs += 1; }
                            Err(e) => log.push(format!("[cosmetic] pickaxe {} NOT installed: {e}", job.id)),
                        }
                    }
                    Ok(Err(e)) => log.push(format!("[cosmetic] pickaxe {} cannot load: {e}; the current tool stays", job.id)),
                    Err(TryRecvError::Empty) => self.pick = Some(job),
                    Err(TryRecvError::Disconnected) => log.push(format!("[cosmetic] pickaxe {}: the loading thread stopped unexpectedly (the current tool stays)", job.id)),
                }
            }
            for l in &log { eprintln!("{l}"); }
            log
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    static RT: Mutex<Option<Runtime>> = Mutex::new(None);
    // BROWSER PORT: the runtime holds GPU models, which belong to the main thread on the web
    #[cfg(target_arch = "wasm32")]
    thread_local! { static RT_WEB: std::cell::RefCell<Option<Runtime>> = const { std::cell::RefCell::new(None) }; }
    /// The app's per-frame hook (before `Scene::update`): one process-wide [`Runtime`].
    pub fn tick(sim: &mut GameSim, scene: &mut Scene) -> Vec<String> {
        #[cfg(target_arch = "wasm32")]
        let log = RT_WEB.with(|r| r.borrow_mut().get_or_insert_with(Runtime::default).tick(sim, scene));
        #[cfg(not(target_arch = "wasm32"))]
        let log = { let mut g = RT.lock().unwrap_or_else(|e| e.into_inner()); g.get_or_insert_with(Runtime::default).tick(sim, scene) };
        log
    }
    /// BROWSER PORT: the GPU half of a skin swap (the worker thread's half on native), on the main thread.
    #[cfg(target_arch = "wasm32")]
    fn web_upload_skin<T: Target + ?Sized>(mut decoded: SkinDecoded, target: &T) -> SkinReady {
        let tu = Instant::now();
        let gpu = target.builder().map(|b| {
            let m = &decoded.model;
            let pal = |i: usize| m.rig_parts.get(i).map(|p| p.palette_len()).unwrap_or(1);
            (b.build(&m.body, pal(0), "swap body"), b.build(&m.head, pal(1), "swap head"))
        });
        decoded.upload_ms = tu.elapsed().as_millis();
        if gpu.is_some() { decoded.model.body = vec![]; decoded.model.head = vec![]; }
        SkinReady { decoded, gpu }
    }
    /// BROWSER PORT: the GPU half of a pickaxe swap, on the main thread.
    #[cfg(target_arch = "wasm32")]
    fn web_upload_pickaxe<T: Target + ?Sized>(mut decoded: PickDecoded, target: &T) -> PickReady {
        let tu = Instant::now();
        let gpu = target.builder().map(|b| b.build(&decoded.prims, 1, "swap pickaxe"));
        decoded.upload_ms = tu.elapsed().as_millis();
        if gpu.is_some() { decoded.prims = vec![]; }
        PickReady { decoded, gpu }
    }
    /// What the runtime has installed: (skin item id, skin part packages, pickaxe item id, pickaxe WID, pickaxe mesh package).
    pub fn installed() -> (Option<String>, Vec<String>, Option<String>, Option<String>, Option<String>) {
        #[cfg(target_arch = "wasm32")]
        let g = RT_WEB.with(|r| r.borrow().as_ref().map(|r| (r.installed_skin.clone(), r.installed_parts.clone(), r.installed_pickaxe.clone(), r.pickaxe.as_ref().map(|p| p.weapon_definition.clone()), r.pickaxe_mesh.clone())));
        #[cfg(target_arch = "wasm32")]
        return g.unwrap_or((None, vec![], None, None, None));
        #[cfg(not(target_arch = "wasm32"))]
        let g = RT.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(not(target_arch = "wasm32"))]
        match g.as_ref() { Some(r) => (r.installed_skin.clone(), r.installed_parts.clone(), r.installed_pickaxe.clone(), r.pickaxe.as_ref().map(|p| p.weapon_definition.clone()), r.pickaxe_mesh.clone()), None => (None, vec![], None, None, None) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use fn_game::cosmetic_swap::{set_pickaxe, set_skin};
        use fn_game::FrameInput;

        /// Records what the swap installs; no GPU.
        #[derive(Default)]
        struct Mock { hero: String, parts: Vec<String>, rig_parts: usize, skin_log: Vec<String>, pick_mesh: String, pick_wid: String, pick_swing: String, pick_log: Vec<String> }
        impl Target for Mock {
            fn builder(&self) -> Option<ModelBuilder> { None }
            fn install_skin(&mut self, r: &mut SkinReady) -> Result<String, String> {
                let d = &r.decoded;
                assert!(d.model.body.iter().all(|p| !p.vertices.is_empty()) && !d.model.body.is_empty() && !d.model.head.is_empty(), "the decoded body / head carry their primitives");
                self.hero = d.hero.clone(); self.parts = d.parts.clone(); self.rig_parts = d.model.rig_parts.len(); self.skin_log.push(d.item.id.clone());
                Ok(d.item.id.clone())
            }
            fn install_pickaxe(&mut self, r: &mut PickReady) -> Result<String, String> {
                let d = &r.decoded;
                assert!(!d.prims.is_empty() && d.prims.iter().all(|p| p.kind == MaterialKind::Prop && p.vertices.iter().all(|v| v.joints == [0; 4])), "rigid prop primitives");
                self.pick_mesh = d.mesh.clone(); self.pick_wid = d.set.weapon_definition.clone(); self.pick_swing = d.set.swing_montage.clone(); self.pick_log.push(d.item.id.clone());
                Ok(d.item.id.clone())
            }
        }
        fn have() -> bool { fn_game::cosmetic_swap::paks().is_ok() }
        /// The frame loop: tick until every job finished (30 s limit); each tick is one app frame.
        fn pump(rt: &mut Runtime, sim: &mut GameSim, m: &mut Mock) -> usize {
            let t = Instant::now(); let mut frames = 0;
            loop { rt.tick(sim, m); frames += 1; if !rt.busy() { return frames; } assert!(t.elapsed().as_secs() < 30, "swap did not finish"); std::thread::sleep(Duration::from_millis(5)); }
        }
        fn sorted(mut v: Vec<String>) -> Vec<String> { v.sort(); v }
        fn expected_parts(hero: &str) -> Vec<String> {
            let set = fn_game::cosmetic_swap::paks().unwrap();
            sorted(fn_pose::br_character::hero_part_packages(&ClipSourceDir::Pak(&set), hero).unwrap().iter().map(|p| p.split('.').next().unwrap().to_owned()).collect())
        }
        fn item(kind: Kind, id: &str) -> Item { fn_game::cosmetic_swap::items(kind).into_iter().find(|i| i.id == id).unwrap_or_else(|| panic!("no {id}")) }

        #[test]
        fn a_skin_swap_rebuilds_the_part_list_from_the_new_hero_and_keeps_the_sim() {
            if !have() { eprintln!("1.8 install not found: skipped"); return }
            let mut sim = GameSim::new();
            for _ in 0..30 { sim.step(&FrameInput { forward: 1., ..Default::default() }, 1. / 60.); }
            let before = (sim.x, sim.y, sim.z, sim.yaw, sim.frames, sim.anim_time);
            let (mut rt, mut m) = (Runtime::default(), Mock::default());
            // nothing selected: nothing happens
            rt.tick(&mut sim, &mut m); assert!(!rt.busy() && m.parts.is_empty());
            let skull = item(Kind::Skin, "CID_030_Athena_Commando_M_Halloween");
            let ghoul = item(Kind::Skin, "CID_029_Athena_Commando_F_Halloween");
            set_skin(&mut sim, "skull trooper").unwrap();
            let t = Instant::now();
            rt.tick(&mut sim, &mut m);
            let first_tick = t.elapsed();
            assert!(rt.busy(), "the rebuild runs on a background thread");
            assert!(first_tick.as_millis() < 100, "the frame that takes the request must not do the load: {first_tick:?}");
            assert!(!sim.cosmetics.has_pending());
            let frames = pump(&mut rt, &mut sim, &mut m);
            eprintln!("skull trooper installed after {frames} polling frames ({:?} total)", t.elapsed());
            assert_eq!(m.hero, skull.hero.clone().unwrap());
            assert_eq!(sorted(m.parts.clone()), expected_parts(&m.hero));
            assert!(m.rig_parts >= 2);
            let skull_parts = sorted(m.parts.clone());
            // second skin: the part list is replaced, none of the previous skin's own parts survives
            set_skin(&mut sim, "ghoul trooper").unwrap();
            pump(&mut rt, &mut sim, &mut m);
            assert_eq!(m.hero, ghoul.hero.clone().unwrap());
            let ghoul_parts = sorted(m.parts.clone());
            assert_eq!(ghoul_parts, expected_parts(&m.hero));
            assert_ne!(skull_parts, ghoul_parts, "the part list changed");
            let old_only: Vec<&String> = skull_parts.iter().filter(|p| !ghoul_parts.contains(p)).collect();
            assert!(!old_only.is_empty(), "the old skin has parts of its own");
            for p in old_only { assert!(!m.parts.contains(p), "stale part {p} survived the swap"); }
            assert_eq!(rt.installed_parts, m.parts);
            assert_eq!(rt.installed_skin.as_deref(), Some("CID_029_Athena_Commando_F_Halloween"));
            assert_eq!(m.skin_log, ["CID_030_Athena_Commando_M_Halloween", "CID_029_Athena_Commando_F_Halloween"]);
            // back to the first one: deterministic, nothing carried over from ghoul
            set_skin(&mut sim, "CID_030_Athena_Commando_M_Halloween").unwrap();
            pump(&mut rt, &mut sim, &mut m);
            assert_eq!(sorted(m.parts.clone()), skull_parts);
            assert_eq!((sim.x, sim.y, sim.z, sim.yaw, sim.frames, sim.anim_time), before, "the swap never touches the sim");
        }

        #[test]
        fn a_request_made_while_a_load_runs_supersedes_it() {
            if !have() { return }
            let mut sim = GameSim::new();
            let (mut rt, mut m) = (Runtime::default(), Mock::default());
            set_skin(&mut sim, "skull trooper").unwrap(); rt.tick(&mut sim, &mut m);
            set_skin(&mut sim, "ghoul trooper").unwrap(); rt.tick(&mut sim, &mut m);
            pump(&mut rt, &mut sim, &mut m);
            assert_eq!(m.skin_log, ["CID_029_Athena_Commando_F_Halloween"], "only the newest request is installed");
        }

        #[test]
        fn a_pickaxe_swap_changes_the_held_mesh_wid_and_swing_data() {
            if !have() { return }
            let mut sim = GameSim::new();
            let (mut rt, mut m) = (Runtime::default(), Mock::default());
            set_pickaxe(&mut sim, "Reaper").unwrap();
            rt.tick(&mut sim, &mut m); assert!(rt.busy());
            pump(&mut rt, &mut sim, &mut m);
            let reaper = (m.pick_mesh.clone(), m.pick_wid.clone(), m.pick_swing.clone());
            eprintln!("Reaper -> {reaper:?}");
            assert!(reaper.1.contains("WID_Harvest_HalloweenScythe"), "{reaper:?}");
            assert_eq!(rt.pickaxe.as_ref().unwrap().weapon_definition, reaper.1);
            set_pickaxe(&mut sim, "DefaultPickaxe").unwrap();
            pump(&mut rt, &mut sim, &mut m);
            eprintln!("DefaultPickaxe -> {:?}", (&m.pick_mesh, &m.pick_wid, &m.pick_swing));
            assert_eq!(m.pick_wid, fn_pose::loadout_melee::DEFAULT_WID);
            assert_eq!(m.pick_mesh, fn_pose::loader::PICK_PKG);
            assert_ne!(m.pick_mesh, reaper.0, "the held mesh changed with the pickaxe");
            assert_eq!(rt.installed_pickaxe.as_deref(), Some("DefaultPickaxe"));
            assert_eq!(m.pick_log, ["HalloweenScythe", "DefaultPickaxe"]);
            // a glider is recorded and logged only
            fn_game::cosmetic_swap::set_glider(&mut sim, "Glider_Voyager").unwrap();
            let log = rt.tick(&mut sim, &mut m);
            assert!(log.iter().any(|l| l.contains("Glider_Voyager")), "{log:?}");
            assert!(!rt.busy());
        }

        /// Every shipped BR pickaxe decodes (Bolt-On included) to a textured rigid mesh with a swing montage; prints mesh / WID / swing / textures.
        #[test]
        fn every_shipped_pickaxe_decodes_to_a_textured_mesh() {
            if !have() { return }
            let set = fn_game::cosmetic_swap::paks().unwrap();
            let mut swings = std::collections::BTreeSet::new();
            let mut meshes = std::collections::BTreeSet::new();
            for it in fn_game::cosmetic_swap::items(Kind::Pickaxe) {
                let d = decode_pickaxe(&set, &it).unwrap_or_else(|e| panic!("{}: {e}", it.id));
                let tex: Vec<(bool, bool, bool, bool)> = d.prims.iter().map(|p| (p.material.diffuse.is_some(), p.material.normal.is_some(), p.material.spec.is_some(), p.material.mask.is_some())).collect();
                eprintln!("{:<22} {} mesh {} swing {} sections (diffuse, normal, spec, mask) {:?} {:?} in {} ms", it.id, d.set.weapon_definition, d.mesh, d.set.swing_montage, tex, d.notes, d.ms);
                assert!(!d.prims.is_empty());
                assert!(tex.iter().any(|t| t.0), "{}: no section has a diffuse texture: {:?}", it.id, d.notes);
                swings.insert(d.set.swing_montage.clone()); meshes.insert(d.mesh.clone());
            }
            eprintln!("distinct swing montages {swings:?}; distinct meshes {}", meshes.len());
            assert!(meshes.len() > 1);
        }

        /// The real `Scene` as the swap target on a headless device (needs `FN_ASSETS` = the asset tree for the sky / ground; skipped otherwise): the skin and pickaxe are swapped through
        /// `Runtime::tick`, the new rig parts are installed, the frame still updates and draws, and the device reports no validation error.
        #[test]
        fn the_scene_installs_a_swapped_skin_and_pickaxe_on_a_real_device() {
            if !have() { return }
            let Some(dir) = fn_live::var_os("FN_ASSETS") else { eprintln!("FN_ASSETS not set: skipped"); return };
            let Ok(gpu) = fn_render::Gpu::headless() else { eprintln!("no GPU adapter: skipped"); return };
            let sink = gpu.install_error_sink();
            let roots = AssetRoots::open(vec![PathBuf::from(dir)]);
            let sky = fn_render::SkyAssets::load(roots.find("dome_optimized.bin").and_then(|f| f.parent()).expect("dome_optimized.bin")).unwrap();
            let format = wgpu::TextureFormat::Bgra8UnormSrgb;
            let renderer = Renderer::new(&gpu.device, &gpu.queue, format, &RendererAssets { sky, ground_texture: roots.load_rgba("T_Grasslands_D.png").unwrap(), ground_mips: None }).unwrap();
            let set = fn_game::cosmetic_swap::paks().unwrap();
            let src = ClipSourceDir::Pak(&set);
            let first = crate::br_character::from_character(&fn_pose::br_character::BrCharacter::load(&src, fn_pose::br_character::DEFAULT_HERO).unwrap());
            let pick = decode_pickaxe(&set, &item(Kind::Pickaxe, "DefaultPickaxe")).unwrap();
            let rig = CharacterRig::new(first.rig_parts.clone());
            let mut scene = Scene::new(renderer, Character::new(rig, Box::new(ClipStore::new())), &first.body, &first.head, &pick.prims);
            let old_bones: Vec<Vec<String>> = scene.character.rig.parts.iter().map(|p| p.tree.names.clone()).collect();
            let mut sim = GameSim::new();
            for _ in 0..5 { sim.step(&FrameInput::default(), 1. / 60.); }
            let before = (sim.x, sim.y, sim.z, sim.yaw);
            let mut rt = Runtime::default();
            set_skin(&mut sim, "ghoul trooper").unwrap();
            set_pickaxe(&mut sim, "Reaper").unwrap();
            let t = Instant::now(); let mut worst = Duration::ZERO; let mut frames = 0;
            while rt.installs < 2 {
                let f = Instant::now(); rt.tick(&mut sim, &mut scene); worst = worst.max(f.elapsed()); frames += 1;
                scene.update(&mut sim);
                assert!(t.elapsed().as_secs() < 60, "the swap did not finish");
                std::thread::sleep(Duration::from_millis(5));
            }
            eprintln!("scene swap done after {frames} frames in {:?}; the slowest frame-side tick (install included) took {worst:?}", t.elapsed());
            let new_bones: Vec<Vec<String>> = scene.character.rig.parts.iter().map(|p| p.tree.names.clone()).collect();
            assert_eq!(new_bones.len(), old_bones.len());
            assert_ne!(new_bones, old_bones, "the rig parts are the new skin's");
            assert_eq!(rt.installed_skin.as_deref(), Some("CID_029_Athena_Commando_F_Halloween"));
            assert!(rt.pickaxe_mesh.as_deref().is_some_and(|m| m != fn_pose::loader::PICK_PKG));
            assert!(scene.character.part_anim.as_ref().is_some_and(|p| p.female), "the swapped-in hero is female");
            assert!(scene.character.clips.clip("Pose_Melee_TwoHanded_CMF").is_some(), "the female animation set joined the clip library");
            assert!(sim.anim.vars.female, "the animation graph runs the female set");
            let target = fn_render::OffscreenTarget::new(&gpu.device, 320, 240, format);
            scene.update(&mut sim);
            scene.draw(&crate::scene::camera_for(&sim, 320, 240), 0., 1. / 60., &fn_render::RenderOptions::default(), &target.view);
            assert_eq!(before, (sim.x, sim.y, sim.z, sim.yaw));
            let errs = sink.take();
            assert!(errs.is_empty() && sink.total() == 0, "GPU validation errors: {errs:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn core() -> Core { Core::new(vec![("a".into(), 1.), ("b".into(), 2.), ("c".into(), 1.)]) }
    #[test] fn fraction_is_proportional_to_the_weights_of_finished_units() {
        let mut c = core();
        c.begin(0, 0.); c.end(0, 1.); assert!((c.fraction(1.) - 0.25).abs() < 1e-6);
        c.begin(1, 1.); c.end(1, 3.); assert!((c.fraction(3.) - 0.75).abs() < 1e-6);
        c.begin(2, 3.); c.end(2, 4.); assert_eq!(c.fraction(4.), 1.);
    }
    #[test] fn a_running_unit_counts_for_nothing_until_it_completes() {
        let mut c = core(); c.begin(1, 0.);
        assert_eq!(c.fraction(0.5), 0.); assert_eq!(c.fraction(1000.), 0., "progress is step-driven, never wall-clock driven (reference behaviour)");
        c.end(1, 1000.); assert!((c.fraction(1000.) - 0.5).abs() < 1e-6);
    }
    #[test] fn the_bar_never_goes_backwards_and_never_shows_100_before_everything_is_done() {
        let mut c = core(); let mut last = 0.;
        for (i, t) in [(0, 0.), (1, 0.1), (2, 0.2)] { c.begin(i, t); }
        for k in 0..50 { let f = c.fraction(k as f32 * 0.05); assert!(f >= last); assert!(f < 1.); last = f; }
        c.end(0, 3.); c.end(1, 3.); assert!(c.fraction(3.) < 1.); c.end(2, 3.); assert_eq!(c.fraction(3.), 1.);
    }
    #[test] fn the_label_is_the_unit_announced_last_and_starting_before_that() {
        let mut c = core(); assert_eq!(c.label(), "Starting"); c.begin(0, 0.); assert_eq!(c.label(), "a"); c.begin(2, 0.1); assert_eq!(c.label(), "c");
        c.begin(2, 0.2); assert_eq!(c.label(), "c", "beginning twice changes nothing"); c.end(2, 1.); assert_eq!(c.label(), "c");
    }
    #[test] fn timings_are_recorded_for_every_finished_unit_including_instant_ones() {
        let mut c = core(); c.begin(0, 0.5); c.end(0, 0.75); c.end(1, 2.); assert_eq!(c.units[0].secs(), Some(0.25)); assert_eq!(c.units[1].secs(), Some(0.), "a unit ended without begin is zero seconds, not missing");
        assert!(c.units[2].secs().is_none());
    }
    #[test] fn the_plan_has_one_unit_per_clip_and_per_renderer_phase_and_no_fake_steps() {
        let names: Vec<String> = (0..7).map(|i| format!("clip{i}")).collect();
        let (units, p) = plan(&Source::Dir, &names);
        assert_eq!(p.clips.len(), 7); assert_eq!(p.renderer.len(), RENDERER_PHASES.len()); assert!(p.paks.is_none());
        let (units2, p2) = plan(&Source::Paks(PathBuf::from("x")), &names); assert!(p2.paks.is_some() && p2.loadout.is_some() && units2.len() == units.len() + 2);
        let mut labels: Vec<&String> = units.iter().map(|u| &u.0).collect(); labels.sort(); let n = labels.len(); labels.dedup(); assert_eq!(labels.len(), n, "labels are unique (the renderer maps announced labels to units)");
        assert!(units.iter().all(|u| u.1 > 0.));
        // the "Compiling shaders" work is no longer one 3 s unit that follows a 98 % bar
        let renderer_w: f32 = p.renderer.iter().map(|&i| units[i].1).sum(); let total: f32 = units.iter().map(|u| u.1).sum();
        assert!(units[p.renderer[2]].1 < 0.2 * total && renderer_w < 0.7 * total + 1.0);
    }
    #[test] fn renderer_phase_labels_match_the_ones_fn_render_announces() {
        let src = include_str!("../../fn-render/src/renderer.rs");
        for (label, _) in RENDERER_PHASES { assert!(src.contains(&format!("progress(\"{label}\")")), "{label} is not announced by Renderer::new_with_progress"); }
        assert_eq!(src.matches("progress(\"").count(), RENDERER_PHASES.len(), "every announced phase has a unit");
    }
    #[test] fn slices_split_at_the_limit_never_inside_a_clip_scope_and_cover_every_command() {
        let mut l = RenderList::new(); let r = fn_ui::layout::Rect::new(0., 0., 4., 4.);
        for _ in 0..5 { l.fill(r, [1.; 4]); } l.push_clip(r); for _ in 0..7 { l.fill(r, [1.; 4]); } l.pop_clip(); for _ in 0..3 { l.fill(r, [1.; 4]); }
        let s = slice_ranges(&l, 4);
        assert_eq!(s.first().unwrap().0, 0); assert_eq!(s.last().unwrap().1, l.cmds.len());
        for w in s.windows(2) { assert_eq!(w[0].1, w[1].0); }
        for &(a, b) in &s { let d = l.cmds[a..b].iter().fold(0i32, |d, c| d + match c { Cmd::PushClip(_) => 1, Cmd::PopClip => -1, _ => 0 }); assert_eq!(d, 0, "slice {a}..{b} is balanced"); }
        assert!(slice_ranges(&RenderList::new(), 4).is_empty());
    }
    #[test] fn progress_wrapper_is_thread_safe_and_reports_timings_in_start_order() {
        let p = Progress::new(vec![("x".into(), 1.), ("y".into(), 1.)]);
        let p2 = p.clone(); std::thread::spawn(move || { p2.begin(1); std::thread::sleep(Duration::from_millis(5)); p2.end(1); }).join().unwrap();
        p.begin(0); p.end(0);
        let t = p.timings(); assert_eq!(t.len(), 2); assert!(t[0].2 <= t[1].2); assert!(p.all_done() && p.fraction() == 1.);
        assert!(t.iter().find(|x| x.0 == "y").unwrap().1 >= 0.004, "{t:?}");
    }
}

