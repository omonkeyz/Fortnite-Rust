//! BROWSER PORT: the in-match world load of the browser build (the wasm32 counterpart of `loader.rs` `build_world`; a child module of `loader`,
//! declared there as `#[cfg(target_arch = "wasm32")] mod web`).
//!
//! WebGPU objects belong to the browser's main thread (with wasm atomics wgpu's types are `!Send`, so the native `build_world`, which builds the
//! renderer and uploads everything on a worker, cannot even compile). The same work is split in two:
//! * **worker threads** (the native pools): mount the paks, the starter loadout, every animation clip, the character / pickaxe models and
//!   their textures, the props' decode (`prepare_kind`), the ground texture + sky meshes, the ground-cover and backdrop data;
//! * **the main thread** ([`WorldBuild::step`], called by `Loader::poll` every frame): the `Renderer` (shader modules, pipelines, targets), the
//!   prop / ground-cover / backdrop uploads and `Scene::new` (the character upload).
//! Units, labels, log lines and the resulting `Scene` are the native load's; only where each step runs differs.
use super::*;
use std::sync::mpsc::{Sender, SyncSender};

/// A decoded prop kind (its CPU mip chains) on its way to the main thread's upload.
type PropMsg = (usize, Result<fn_render::static_loader::PreparedProp, String>);
/// MEMORY (2026-10-07): at most this many decoded kinds wait for their upload (the native loader's `sync_channel(6)`). Unbounded, the decode pool
/// ran ahead of the main thread (which uploads only once the renderer exists) and the held mip chains of the full map's kinds filled the 4 GiB wasm
/// heap during the start-up screen (out of memory in `build_mips`, the page stuck).
const PROPS_IN_FLIGHT: usize = 6;

/// MEMORY: the live heap at a step of the load (console), for the 4 GiB wasm32 budget.
fn heap(step: &str) { let (live, peak, mem, _) = fn_web_rt::mem::stats(); eprintln!("[fnweb-heap] {step}: live {} MB, peak {} MB, wasm memory {} MB", live >> 20, peak >> 20, mem >> 20); }

/// The ground cover's CPU half (`foliage::load` before `upload_prepared`).
pub(super) struct FoliageCpu { spec: crate::foliage::Spec, blades: Vec<crate::foliage::Blade>, prep: fn_render::static_loader::PreparedProp, records: usize, started: Instant }

/// Everything the worker side produces for the final `Scene::new`.
pub(super) struct CpuWorld {
    rig: CharacterRig,
    body: Vec<fn_render::SkinnedPrimitive>,
    head: Vec<fn_render::SkinnedPrimitive>,
    pick: Vec<fn_render::SkinnedPrimitive>,
    clips: Box<dyn ClipLibrary + Send>,
    loadout: Option<fn_pose::loadout::Resources>,
}

pub(super) enum Msg {
    RendererAssets(Result<RendererAssets, String>),
    Foliage(Option<FoliageCpu>),
    Backdrop(Option<Result<(fn_render::backdrop::BackdropData, fn_render::backdrop::BackdropPrep), String>>),
    World(Result<CpuWorld, String>),
}

const DEFERRED_BYTES_PER_FRAME: usize = 16 << 20;

/// The main-thread side of the world load.
pub(super) struct WorldBuild {
    rx: Receiver<Msg>,
    props_rx: Receiver<PropMsg>,
    result: Sender<Result<Box<Scene>, String>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    progress: Arc<Progress>,
    plan: Plan,
    assets: Option<RendererAssets>,
    renderer: Option<Renderer>,
    lib: Option<Arc<crate::props::Library>>,
    prop_ids: Vec<String>,
    props_done: usize,
    cache: HashMap<PathBuf, fn_render::StaticMaterialHandle>,
    meshes: Vec<Option<fn_render::StaticMeshHandle>>,
    skipped: Vec<(String, String)>,
    foliage: Option<Option<FoliageCpu>>,
    grass: Option<Option<crate::foliage::Grass>>,
    backdrop: Option<Option<Result<(fn_render::backdrop::BackdropData, fn_render::backdrop::BackdropPrep), String>>>,
    backdrop_applied: bool,
    world: Option<CpuWorld>,
    finished: bool,
}

impl WorldBuild {
    /// Starts the worker side (one `load-world` thread with the native pools inside) and returns the main-thread side.
    pub(super) fn start(job: Job, p: Arc<Progress>, plan: Plan, result: Sender<Result<Box<Scene>, String>>) -> WorldBuild {
        let Job { source, roots, device, queue, format, clip_names, props: props_job } = job;
        let (tx, rx) = channel::<Msg>();
        let (ptx, props_rx) = std::sync::mpsc::sync_channel::<PropMsg>(PROPS_IN_FLIGHT);
        let lib = props_job.as_ref().map(|j| j.lib.clone());
        let prop_ids: Vec<String> = props_job.as_ref().map(|j| crate::props::select_kinds(&j.cat, &j.kinds).iter().map(|k| k.id.clone()).collect()).unwrap_or_default();
        let meshes = lib.as_ref().map(|l| vec![None; l.protos.len()]).unwrap_or_default();
        let (p2, plan2) = (p.clone(), plan.clone());
        std::thread::Builder::new().name("load-world".into()).spawn(move || {
            let r = cpu_world(source, roots, clip_names, props_job, p2, plan2, tx.clone(), ptx);
            let _ = tx.send(Msg::World(r));
        }).ok();
        WorldBuild {
            rx, props_rx, result, device, queue, format, progress: p, plan, assets: None, renderer: None, lib, prop_ids, props_done: 0,
            cache: HashMap::new(), meshes, skipped: vec![], foliage: None, grass: None, backdrop: None, backdrop_applied: false, world: None, finished: false,
        }
    }

    fn fail(&mut self, e: String) { if !self.finished { self.finished = true; let _ = self.result.send(Err(e)); } }

    /// One frame of main-thread work (GPU creation / uploads) within `budget` (a renderer build is one call and may take longer).
    pub(super) fn step(&mut self, budget: Duration) {
        if self.finished { return; }
        let t0 = Instant::now();
        if self.backdrop_applied && !fn_render::texture::pump_deferred(&self.queue, DEFERRED_BYTES_PER_FRAME) { return; }
        loop {
            match self.rx.try_recv() {
                Ok(Msg::RendererAssets(Ok(a))) => self.assets = Some(a),
                Ok(Msg::RendererAssets(Err(e))) => return self.fail(e),
                Ok(Msg::Foliage(f)) => self.foliage = Some(f),
                Ok(Msg::Backdrop(b)) => self.backdrop = Some(b),
                Ok(Msg::World(Ok(w))) => self.world = Some(w),
                Ok(Msg::World(Err(e))) => return self.fail(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => { if self.world.is_none() { return self.fail("the loading thread stopped unexpectedly".into()); } break; }
            }
        }
        // 1. the renderer: one call into the driver per phase, each announced before it runs (as the native renderer thread does)
        if self.renderer.is_none() {
            let Some(assets) = self.assets.take() else { return };
            let p = self.progress.clone();
            let units = self.plan.renderer.clone();
            let mut current: Option<usize> = None;
            let r = Renderer::new_with_progress(&self.device, &self.queue, self.format, &assets, &mut |label| {
                if let Some(c) = current.take() { p.end(c); }
                if let Some(i) = units.iter().copied().find(|&i| p.with(|c, _| c.units[i].label == label)) { p.begin(i); current = Some(i); }
            });
            if let Some(c) = current.take() { self.progress.end(c); }
            match r { Ok(r) => self.renderer = Some(r), Err(e) => return self.fail(e) }
            heap("renderer built");
            return; // the renderer build is this frame's work
        }
        let renderer = self.renderer.as_mut().expect("checked");
        // 2. the decoded prop kinds, one progress unit each (a kind that fails stays a procedural placeholder); taken from the bounded channel only
        //    here, so the decode pool waits instead of filling the heap
        while self.props_done < self.prop_ids.len() {
            let (i, r) = match self.props_rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => { self.props_done = self.prop_ids.len(); break; } // the pool ended early: the rest stay placeholders
            };
            let unit = self.plan.props.get(i).copied();
            if let Some(u) = unit { self.progress.begin(u); }
            match r {
                Ok(prep) => {
                    let id = prep.id.clone();
                    let lp = fn_render::static_loader::upload_prepared(renderer, prep, &mut self.cache);
                    if let Some(pi) = self.lib.as_ref().and_then(|l| l.proto_index(&id)) { self.meshes[pi as usize] = Some(lp.mesh); }
                }
                Err(e) => self.skipped.push((self.prop_ids.get(i).cloned().unwrap_or_default(), e)),
            }
            if let Some(u) = unit { self.progress.end(u); }
            self.props_done += 1;
            if t0.elapsed() >= budget { return; }
        }
        if self.props_done < self.prop_ids.len() { return; }
        if self.props_done == self.prop_ids.len() && self.grass.is_none() && self.foliage.is_some() { heap("props uploaded"); }
        for &u in &self.plan.props { self.progress.end(u); }
        // 3. the ground cover (FOLIAGE HOOK: foliage::load's GPU half)
        if self.grass.is_none() {
            let Some(f) = self.foliage.take() else { return };
            let u = self.plan.foliage;
            if let Some(u) = u { self.progress.begin(u); }
            self.grass = Some(f.map(|f| {
                let loaded = fn_render::static_loader::upload_prepared(renderer, f.prep, &mut HashMap::new());
                let mut grass = crate::foliage::Grass::new(loaded.mesh, f.spec.cull_m(), f.blades);
                if let (Some(mesh), Some(material)) = (f.spec.mesh_ue_path.clone(), f.spec.material_ue_path.clone()) {
                    grass.source = Some(crate::foliage::GrassSource { mesh_ue_path: mesh, material_ue_path: material, start_cull_cm: f.spec.start_cm, end_cull_cm: f.spec.end_cm, cast_shadow: f.spec.cast_shadow });
                }
                eprintln!("[foliage] {}: {} instances of the map's {} placed in the baseplate, cull {:.1} m (fade {:.1} m), {:.0} ms", f.spec.name, grass.total(), f.records, f.spec.cull_m(), (f.spec.end_cm - f.spec.start_cm) * 0.01, f.started.elapsed().as_secs_f32() * 1000.);
                grass
            }));
            if let Some(u) = u { self.progress.end(u); }
            if t0.elapsed() >= budget { return; }
        }
        // 4. the backdrop (BACKDROP HOOK: renderer.set_backdrop)
        if !self.backdrop_applied {
            let Some(b) = self.backdrop.take() else { return };
            match b {
                Some(Ok((d, prep))) => { crate::storm_hook::set_ground_ref(d.ground_ref_cm); renderer.set_backdrop_prepared(&d, prep) }
                Some(Err(e)) => eprintln!("[backdrop] {e}: no landscape / ocean"),
                None => {}
            }
            self.backdrop_applied = true;
            return;
        }
        if !fn_render::texture::pump_deferred(&self.queue, DEFERRED_BYTES_PER_FRAME) { return; }
        let Some(w) = self.world.take() else { return };
        let renderer = self.renderer.take().expect("checked");
        match assemble(renderer, w, &self.progress, &self.plan, self.lib.take(), std::mem::take(&mut self.meshes), std::mem::take(&mut self.skipped), self.grass.take().flatten()) {
            Ok(scene) => {
                heap("scene assembled");
                self.finished = true;
                let _ = self.result.send(Ok(scene));
            }
            Err(e) => self.fail(e),
        }
    }
}

/// `build_world` steps 4.: the scene from the uploaded renderer + the worker's character / clips (native code, unchanged).
fn assemble(renderer: Renderer, w: CpuWorld, p: &Progress, plan: &Plan, lib: Option<Arc<crate::props::Library>>, meshes: Vec<Option<fn_render::StaticMeshHandle>>, skipped: Vec<(String, String)>, grass: Option<crate::foliage::Grass>) -> Result<Box<Scene>, String> {
    p.begin(plan.assemble);
    let CpuWorld { rig, body, head, pick, clips, loadout } = w;
    let mut scene = Scene::new(renderer, Character::new(rig, clips), &body, &head, &pick);
    scene.character.part_anim = crate::part_anim_glue::take(); // PART ANIM HOOK
    scene.real = lib.map(|lib| crate::props::Real { lib, meshes, skipped });
    scene.grass = grass; // FOLIAGE HOOK
    if let Some(resources) = &loadout {
        let hand = fn_pose::SocketDef::from_original_yaw_socket(0, &resources.player_hand_socket)?;
        if scene.character.rig.parts[0].tree.bone_index(&hand.bone).is_none() { return Err("original hand socket bone missing from character".into()); }
        scene.character.rig.sockets = vec![hand];
        eprintln!("[loadout] installed original player RightHand socket for final-pose weapon attachment");
    }
    scene.loadout = loadout;
    p.end(plan.assemble);
    Ok(Box::new(scene))
}

/// The worker side: `build_world` without its GPU calls (same order, same pools, same units); GPU-bound results go to the main thread as messages.
fn cpu_world(source: Source, roots: Arc<AssetRoots>, clip_names: Vec<String>, props_job: Option<PropsJob>, p: Arc<Progress>, plan: Plan, tx: Sender<Msg>, ptx: SyncSender<PropMsg>) -> Result<CpuWorld, String> {
    // 1. mount (pak mode)
    let paks = match &source {
        Source::Paks(dir) => {
            let i = plan.paks.ok_or("no pak unit")?; p.begin(i);
            let set = Arc::new(fn_pak::PakSet::open_install(dir).map_err(|e| format!("Cannot open the paks in {}: {e}", dir.display()))?);
            p.end(i); Some(set)
        }
        Source::Dir => None,
    };
    heap("paks mounted");
    let loadout = if let Some(set) = &paks {
        let i = plan.loadout.ok_or("no starter loadout progress unit")?;
        p.begin(i);
        let mut resources = fn_pose::loadout::Resources::load(&ClipSourceDir::Pak(set))?;
        crate::emote_hook::absorb(set, &mut resources);
        eprintln!("[loadout] loaded {} original item definitions, {} role bindings, {} montages, {} composites, {} sequences", resources.items.len(), resources.bindings.len(), resources.montages.len(), resources.composites.len(), resources.sequences.len());
        p.end(i);
        Some(resources)
    } else { None };
    // 2. the clip loader shared by the pool
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
    let (ctx, crx) = channel::<(usize, Result<Loaded, String>)>();
    let prop_kinds: Vec<&fn_propdata::PropKind> = props_job.as_ref().map(|j| crate::props::select_kinds(&j.cat, &j.kinds)).unwrap_or_default();
    let next_prop = std::sync::atomic::AtomicUsize::new(0);
    let (rig, body, head, pickp) = std::thread::scope(|s| -> Result<_, String> {
        // prop decode pool -> main thread
        for _ in 0..workers().min(4) {
            let (ptx, kinds, next) = (ptx.clone(), &prop_kinds, &next_prop);
            s.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if i >= kinds.len() { break; }
                let known = std::collections::HashSet::new();
                let r = fn_render::static_loader::prepare_kind(kinds[i], &known);
                if ptx.send((i, r)).is_err() { break; }
            });
        }
        drop(ptx);
        // clip pool
        for _ in 0..workers() {
            let (queue_idx, ctx, job, p, names, units) = (queue_idx.clone(), ctx.clone(), clip_job.clone(), p.clone(), &clip_names, &plan.clips);
            s.spawn(move || loop {
                let Some(i) = queue_idx.lock().ok().and_then(|mut q| q.pop_front()) else { break };
                p.begin(units[i]);
                let r = job(&names[i]);
                p.end(units[i]);
                if ctx.send((i, r)).is_err() { break; }
            });
        }
        drop(ctx);
        // the renderer's CPU inputs (ground texture + sky meshes) -> main thread
        {
            let (p, roots, tex_unit, tx) = (p.clone(), roots.clone(), plan.texture, tx.clone());
            s.spawn(move || {
                let r = (|| -> Result<RendererAssets, String> {
                    p.begin(tex_unit);
                    let ground = roots.load_rgba("T_Grasslands_D.png")?;
                    p.end(tex_unit);
                    let sky_dir = roots.find("dome_optimized.bin").and_then(|f| f.parent()).ok_or("Missing asset: dome_optimized.bin")?;
                    let mut sky = SkyAssets::load(sky_dir)?;
                    sky.prepare_mips();
                    let ground_mips = Some(fn_render::texture::mip_chain(&ground, true, fn_render::texture::MipFilter::Triangle));
                    Ok(RendererAssets { sky, ground_texture: ground, ground_mips })
                })();
                let _ = tx.send(Msg::RendererAssets(r));
            });
        }
        // this thread: models, then the characters' textures
        p.begin(plan.models);
        let (rig, pick) = parts_from(&source, &roots, paks.as_ref())?;
        p.end(plan.models);
        p.begin(plan.char_tex);
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
        Ok((rig, body, head, pickp))
    })?;
    heap("clips / models / prop decode started");
    // FOLIAGE HOOK, CPU half (foliage::load up to upload_prepared)
    let foliage = props_job.as_ref().and_then(|j| {
        let started = Instant::now();
        let spec = crate::foliage::Spec::read(&j.cat.dir).map_err(|e| eprintln!("[foliage] {e}: no ground cover")).ok()?;
        let bytes = std::fs::read(&spec.instances_file).map_err(|e| eprintln!("[foliage] {}: {e}: no ground cover", spec.instances_file.display())).ok()?;
        let blades = crate::foliage::read_instances(&bytes, [j.lib.center_cm[0], j.lib.center_cm[1]], crate::props::ANCHOR, fn_game::HALF - 1.).map_err(|e| eprintln!("[foliage] {e}: no ground cover")).ok()?;
        let gc = fn_render::static_loader::GroundCoverSpec { name: &spec.name, gltf: &spec.gltf, materials: &spec.materials, bounds_extent_cm: spec.bounds_extent_cm, cull_cm: spec.end_cm, fade_cm: spec.end_cm - spec.start_cm, cast_shadow: spec.cast_shadow };
        let prep = fn_render::static_loader::prepare_ground_cover(&gc).map_err(|e| eprintln!("[foliage] {}: {e}: no ground cover", spec.name)).ok()?;
        let records = (bytes.len().saturating_sub(12)) / crate::foliage::RECORD_BYTES;
        Some(FoliageCpu { spec, blades, prep, records, started })
    });
    let _ = tx.send(Msg::Foliage(foliage));
    heap("ground cover");
    // BACKDROP HOOK, CPU half
    let backdrop = crate::backdrop_load::find_dir(&roots.roots).map(|dir| {
        if let Ok(c) = crate::backdrop_load::read_centre(&dir) { crate::storm_hook::set_mapping(c, crate::props::ANCHOR); }
        crate::backdrop_load::read_centre(&dir).and_then(|c| crate::backdrop_load::load(&dir, c, crate::props::ANCHOR, fn_game::HALF))
    });
    heap("backdrop");
    let ground_ref_cm = backdrop.as_ref().and_then(|r| r.as_ref().ok()).map(|d| d.ground_ref_cm);
    let backdrop = backdrop.map(|r| r.map(|mut d| { let prep = fn_render::backdrop::prepare(&d); for m in &mut d.meshes { m.verts = Vec::new(); m.indices = Vec::new(); } (d, prep) }));
    let _ = tx.send(Msg::Backdrop(backdrop));
    // POI (native build_world): every POI sublevel's actors (statics_render draws them, streamed; their collision / building data go to the props)
    if let Some(j) = props_job.as_ref().filter(|j| j.poi) {
        let c = crate::poi::world_center().unwrap_or(j.lib.center_cm);
        crate::poi::load(&j.cat.dir, c, if c[2] != 0. { c[2] } else { ground_ref_cm.unwrap_or(0.) });
    }
    heap("POI");
    // 3. collect the clips
    let total = clip_names.len();
    let mut md5: HashMap<String, Md5Clip> = HashMap::new();
    let mut fnclips = ClipStore::new();
    let mut got = 0;
    while got < total {
        match crx.recv() {
            Ok((i, r)) => {
                let name = &clip_names[i];
                match r.map_err(|e| format!("Cannot load animation {name}: {e}"))? { Loaded::Md5(c) => { md5.insert(name.clone(), c); } Loaded::Fn(c) => { fnclips.insert(name, *c); } }
                got += 1;
            }
            Err(_) => return Err("the animation workers stopped unexpectedly".into()),
        }
    }
    if let Some(resources) = &loadout {
        let added = resources.install_player_clips(&mut fnclips);
        eprintln!("[loadout] installed {added} additional original player clips; weapon-skeleton sequences remain separate");
    }
    crate::br_character::take_female_clips(&mut fnclips);
    let clips: Box<dyn ClipLibrary + Send> = match source { Source::Dir => Box::new(md5), Source::Paks(_) => Box::new(fnclips) };
    heap("clips collected");
    let (mut body, mut head, mut pickp) = (body, head, pickp);
    for prims in [&mut body, &mut head, &mut pickp] { fn_render::mesh::prepare_mip_chains(prims); }
    Ok(CpuWorld { rig, body, head, pick: pickp, clips, loadout })
}
