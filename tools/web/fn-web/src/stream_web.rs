//! BROWSER PORT (wasm32 only; a child module of `statics_render`): POI STREAMING without network waits on the page's main thread.
//!
//! Natively `StaticsRender::poi_build` builds a newly resident POI group's kinds on the render thread (mesh package, distance field, slot materials,
//! their shader maps and textures: ~50 ms per kind from the memory-mapped paks). In the browser every one of those reads is a synchronous request
//! over the internet (remote mode: a round trip each, tens of them per kind) made while the main thread spins: walking into a town dropped the game
//! to ~5 fps. Here the same reads happen on worker threads first, AHEAD of the player:
//! * [`request_ring`]: the kinds of every group within [`RING`] x `poi::stream_cm` of the view (nearest first) are queued;
//! * [`ready`]: `poi_build` builds a kind only once its reads are done (else it asks for it at the front of the queue and skips it this frame:
//!   the group appears a moment later instead of the frame rate collapsing);
//! * a worker lane reads the kind's packages inside `fn_pak::retain_reads` (the mesh, its distance field, every slot material's chain + shader map,
//!   their textures' packages), parses the slot materials (`Material::load`: 100-170 ms of CPU each, the same call `entry` makes) into
//!   `statics_render`'s preloaded map, and decodes the textures (`fn_fx::gpu::predecode_texture`); the main thread then only builds GPU objects.
//! Kinds the main thread asks for go first and are always read; the ring's read-ahead stops while [`AHEAD`] kinds wait read-but-unbuilt, so the
//! retained reads (an LRU of 96 MB) are not evicted before they are used (urgent requests never wait for that cap: `poi_build` only consumes the
//! nearest incomplete group, so a cap on everything could stall on read-ahead it never consumes).
//! Same kinds, same data, same build order on the main thread: only where the bytes come from changes.
use fn_pak::PakSet;
use glam::Vec3;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Prefetch ring: groups within this many times the POI residency distance.
const RING: f32 = 1.5;
/// Worker lanes reading kinds (each a pool worker for the session): the machine's threads minus the game's own busy ones, 4..12. A kind's reads +
/// material parses are CPU-heavy in wasm (~1-3 s each) and the groups around the spawn hold ~1000 kinds.
fn lanes() -> usize { (fn_web_rt::js::fnweb_hardware_concurrency() as usize).saturating_sub(2).clamp(4, 12) } // (std reports at most 8 in the browser build)
/// Kinds read but not yet built by the main thread, at most, before the resident groups' / the ring's read-ahead pauses (the retained reads of more
/// would be evicted from fn-pak's LRU before the main thread uses them, and it would read them again itself: seen as 250 ms per kind and GBs of
/// re-reads). What the main thread asks for this frame is always read.
const AHEAD: usize = 96;
const LOOK_AHEAD_S: f32 = 6.;
const URGENT_AHEAD: usize = 4 * AHEAD;
const URGENT_TEX_BYTES: usize = 256 << 20;
const AHEAD_TEX_BYTES: usize = 96 << 20;
const KEEP: f32 = 1.5;

#[derive(Default)]
struct State {
    /// kinds queued, being read or read (never queued twice)
    known: HashSet<String>,
    /// kinds whose reads are done (they stay: `ready` may be asked again for a kind `poi_build` did not get to build within its budget)
    done: HashSet<String>,
    /// done kinds the main thread was told about (counted out of [`UNBUILT`] once)
    consumed: HashSet<String>,
    /// asked for by the main thread this frame (read first, never capped)
    now: VecDeque<String>,
    /// the resident groups' kinds, nearest group first (capped by [`AHEAD`])
    urgent: VecDeque<String>,
    /// the ring's read-ahead (capped by [`AHEAD`])
    queue: VecDeque<String>,
    /// slot materials already parsed by a lane (each handed out once through the preloaded map)
    mats: HashSet<String>,
    parsing: HashSet<String>,
    meshes: HashSet<String>,
    /// kinds a lane is reading now, with when it started (diagnostics: `[poi prefetch] status`)
    reading: std::collections::HashMap<String, std::time::Instant>,
    queued: HashMap<String, u8>,
    claims: HashMap<String, Claim>,
    mat_tex: HashMap<String, Vec<String>>,
}

#[derive(Default)]
struct Claim { own_mats: Vec<String>, own_meshes: Vec<String>, used_mats: Vec<String>, used_tex: Vec<String>, used_meshes: Vec<String> }
static STATE: Mutex<Option<State>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();
static UNBUILT: AtomicUsize = AtomicUsize::new(0);
static SET: OnceLock<Arc<PakSet>> = OnceLock::new();

/// `?FNWEB_POI_PREFETCH=0`: off (the main thread reads the kinds itself, as natively; for measurements).
fn enabled() -> bool { static ON: OnceLock<bool> = OnceLock::new(); *ON.get_or_init(|| std::env::var("FNWEB_POI_PREFETCH").map_or(true, |v| v != "0")) }

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

fn start(set: &Arc<PakSet>) {
    if SET.set(set.clone()).is_err() { return; }
    fn_fx::gpu::set_upgrade_paks(set.clone());
    for i in 0..lanes() {
        let _ = std::thread::Builder::new().name(format!("poi prefetch {i}")).spawn(lane);
    }
}

fn lane() {
    let Some(set) = SET.get().cloned() else { return };
    loop {
        let id = {
            let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let st = g.get_or_insert_with(State::default);
                if let Some(id) = st.now.pop_front() { if st.queued.get(&id) == Some(&0) { st.queued.remove(&id); break id; } continue; }
                // read-ahead only while the main thread keeps up (the retained reads must stay in their LRU)
                if UNBUILT.load(Ordering::Relaxed) < URGENT_AHEAD && fn_fx::gpu::predecoded_bytes() < URGENT_TEX_BYTES { if let Some(id) = st.urgent.pop_front() { if st.queued.get(&id) == Some(&1) { st.queued.remove(&id); break id; } continue; } }
                if UNBUILT.load(Ordering::Relaxed) < AHEAD && fn_fx::gpu::predecoded_bytes() < AHEAD_TEX_BYTES { if let Some(id) = st.queue.pop_front() { if st.queued.get(&id) == Some(&2) { st.queued.remove(&id); break id; } continue; } }
                g = WAKE.wait_timeout(g, std::time::Duration::from_millis(50)).map(|r| r.0).unwrap_or_else(|e| e.into_inner().0);
            }
        };
        let t0 = std::time::Instant::now();
        with(|s| { s.reading.insert(id.clone(), t0); });
        read_kind(&set, &id);
        with(|s| { s.reading.remove(&id); });
        let ms = t0.elapsed().as_secs_f32() * 1000.;
        if ms > 2000. { eprintln!("[poi prefetch] {id}: {ms:.0} ms"); }
        UNBUILT.fetch_add(1, Ordering::Relaxed);
        with(|s| { s.done.insert(id); });
    }
}

/// The reads of `StaticsRender::kind` + `prebuild` for one POI kind (errors are left for the main thread to report).
fn read_kind(set: &PakSet, id: &str) {
    let Some((mesh, overrides)) = crate::poi::kind_source(id) else { return };
    let mut claim = Claim::default();
    fn_pak::retain_reads(|| {
        let long = fn_fx::data::long_package(&mesh).to_owned();
        let key = super::mesh_key(id, &mesh);
        let fresh = with(|st| st.meshes.insert(key.clone()));
        claim.used_meshes.push(key.clone());
        let prep = super::prep_mesh(set, id, &mesh, &long);
        let Ok(pm) = &prep else { return };
        let mats = pm.mats.clone();
        if fresh {
            claim.own_meshes.push(key.clone());
            if let Ok(mut g) = super::PREPPED.lock() { g.get_or_insert_with(Default::default).insert(key, prep); }
            let d = fn_fx::gpu::load_mesh_distance_field(set, &mesh, true); if let Ok(mut g) = super::PREPPED_DF.lock() { g.get_or_insert_with(Default::default).insert(format!("{mesh}|true"), d); }
        }
        let nslots = mats.len().max(overrides.len());
        let mid = crate::poi::kind_mid0(id);
        let mid_base = mid.as_ref().and_then(|x| x.0.clone().or_else(|| mats.first().cloned().flatten()));
        let want_tex = |t: String, claim: &mut Claim| { if !claim.used_tex.contains(&t) { claim.used_tex.push(t); } };
        if let Some(mid) = &mid { for (_, t) in &mid.1 { want_tex(t.clone(), &mut claim); } }
        for s in 0..nslots + mid_base.is_some() as usize {
            let path = if s == nslots { mid_base.clone() } else { overrides.get(s).cloned().flatten().or_else(|| mats.get(s).cloned().flatten()) };
            let Some(path) = path else { continue };
            if claim.used_mats.contains(&path) { continue; }
            claim.used_mats.push(path.clone());
            if !with(|st| { let fresh = st.mats.insert(path.clone()); if fresh { st.parsing.insert(path.clone()); } fresh }) { continue; }
            claim.own_mats.push(path.clone());
            let r = fn_fx::material::Material::load(set, &path).map(Arc::new);
            if let Ok(mat) = &r {
                let _ = fn_fx::data::package_files(set, &mat.shader_map_package);
                if let Some(ps) = super::PS_TYPES.iter().find(|ps| mat.cooked.find(fn_fx::gpu::Vf::Local.type_name(), ps).is_some()) { fn_fx::gpu::preassemble(mat, fn_fx::gpu::Vf::Local, super::VS_TYPE, ps, super::instancing() && matches!(mat.flags.blend.as_str(), "BLEND_Opaque" | "BLEND_Masked")); }
                let tex: Vec<String> = (0..mat.uniform.textures2d.len()).filter_map(|k| mat.texture_slot(k).map(|x| x.0)).collect();
                for t in &tex { want_tex(t.clone(), &mut claim); }
                with(|st| { st.mat_tex.insert(path.clone(), tex); });
            }
            if let Ok(mut g) = super::PRELOADED.lock() { g.get_or_insert_with(Default::default).entry(path.clone()).or_insert(r); }
            with(|st| { st.parsing.remove(&path); });
            WAKE.notify_all();
        }
    });
    {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let st = g.get_or_insert_with(State::default);
            if !claim.used_mats.iter().any(|m| st.parsing.contains(m)) { break; }
            g = WAKE.wait_timeout(g, std::time::Duration::from_millis(20)).map(|r| r.0).unwrap_or_else(|e| e.into_inner().0);
        }
    }
    with(|st| { for m in &claim.used_mats { if let Some(v) = st.mat_tex.get(m) { for t in v { if !claim.used_tex.contains(t) { claim.used_tex.push(t.clone()); } } } } });
    for t in &claim.used_tex {
        if fn_fx::gpu::is_uploaded(t) { continue; }
        fn_fx::gpu::predecode_texture(set, t);
        if !fn_fx::gpu::is_predecoded(t) { eprintln!("[texrefused] {t}"); }
    }
    with(|st| { st.claims.insert(id.to_owned(), claim); });
}

fn release_all<'a>(s: &mut State, ids: impl Iterator<Item = &'a str>) {
    let gone: Vec<Claim> = ids.filter_map(|id| s.claims.remove(id)).collect();
    if gone.is_empty() { return; }
    let (mut mat_heir, mut mesh_heir, mut tex_used, mut base_used): (HashMap<String, String>, HashMap<String, String>, HashSet<String>, HashSet<String>) = Default::default();
    for (k, v) in &s.claims {
        for m in &v.used_mats { mat_heir.entry(m.clone()).or_insert_with(|| k.clone()); }
        for m in &v.used_meshes { mesh_heir.entry(m.clone()).or_insert_with(|| k.clone()); base_used.insert(m.split('#').next().unwrap_or(m).to_owned()); }
        for t in &v.used_tex { tex_used.insert(t.clone()); }
    }
    for c in gone {
        for m in c.own_mats {
            match mat_heir.get(&m) {
                Some(h) => { if let Some(v) = s.claims.get_mut(h) { v.own_mats.push(m); } }
                None => { s.mats.remove(&m); if let Ok(mut g) = super::PRELOADED.lock() { if let Some(map) = g.as_mut() { map.remove(&m); } } }
            }
        }
        for t in c.used_tex {
            if !tex_used.contains(&t) { fn_fx::gpu::drop_predecoded(std::slice::from_ref(&t)); }
        }
        for k in c.own_meshes {
            match mesh_heir.get(&k) {
                Some(h) => { if let Some(v) = s.claims.get_mut(h) { v.own_meshes.push(k); } }
                None => {
                    s.meshes.remove(&k);
                    if let Ok(mut g) = super::PREPPED.lock() { if let Some(map) = g.as_mut() { map.remove(&k); } }
                    let mesh = k.split('#').next().unwrap_or(&k).to_owned();
                    if !base_used.contains(&mesh) {
                        if let Ok(mut g) = super::PREPPED_DF.lock() { if let Some(map) = g.as_mut() { map.remove(&format!("{mesh}|true")); } }
                    }
                }
            }
        }
    }
}

pub(super) fn built(id: &str) { if enabled() { with(|s| { s.claims.remove(id); }); } }

/// `tier`: 0 = the main thread asks now, 1 = a resident group's kind, 2 = the ring's read-ahead.
fn queue(set: &Arc<PakSet>, id: &str, tier: u8) {
    start(set);
    if with(|s| queue_in(s, id, tier)) { WAKE.notify_one(); }
}

fn queue_in(s: &mut State, id: &str, tier: u8) -> bool {
    if s.done.contains(id) { return false; }
    if let Some(cur) = s.queued.get_mut(id) {
        if *cur <= tier { return false; }
        *cur = tier;
    } else {
        if s.known.contains(id) { return false; }
        s.known.insert(id.to_owned());
        s.queued.insert(id.to_owned(), tier);
    }
    match tier { 0 => s.now.push_back(id.to_owned()), 1 => s.urgent.push_back(id.to_owned()), _ => s.queue.push_back(id.to_owned()) }
    true
}

/// `poi_build` is about to build `id` on the main thread: true once its reads are done (then it counts as built), else it is asked for first.
fn textures_ready(set: &Arc<PakSet>, id: &str) -> bool {
    static FETCHING: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let missing: Vec<String> = with(|s| s.claims.get(id).map(|c| c.used_tex.iter().filter(|t| !fn_fx::gpu::is_uploaded(t) && !fn_fx::gpu::is_predecoded(t)).cloned().collect()).unwrap_or_default());
    if missing.is_empty() { return true; }
    let start = FETCHING.lock().map(|mut g| g.get_or_insert_with(HashSet::new).insert(id.to_owned())).unwrap_or(false);
    if start {
        let (set, id) = (set.clone(), id.to_owned());
        let spawned = std::thread::Builder::new().name("poi texture fetch".into()).spawn(move || {
            for t in &missing { fn_fx::gpu::predecode_texture_urgent(&set, t); }
            if let Ok(mut g) = FETCHING.lock() { if let Some(m) = g.as_mut() { m.remove(&id); } }
        });
        if spawned.is_err() { return true; }
    }
    false
}

pub(super) fn ready(set: &Arc<PakSet>, id: &str, promote: bool) -> bool {
    if !enabled() { return true; }
    let (done, first, known, queued) = with(|s| {
        let d = s.done.contains(id);
        let f = d && s.consumed.insert(id.to_owned());
        (d, f, s.known.contains(id), s.queued.get(id).is_some_and(|&t| t > 0))
    });
    if first { let _ = UNBUILT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_sub(1))); fn_web_rt::POI_KINDS_BUILT.fetch_add(1, Ordering::Relaxed); }
    if done { return textures_ready(set, id); }
    if promote { status_log(id); }
    if known && (!promote || !queued) { return false; }
    queue(set, id, 0);
    false
}

/// Queues the kinds of the groups around `eye` (FX space cm), nearest group first: the RESIDENT groups' kinds as urgent (`poi_build` asks only for
/// its nearest incomplete group, which would leave the lanes idle on the rest), the ring's as capped read-ahead. Cheap when the view has not moved
/// 10 m since the last call (and the resident set did not change).
pub(super) fn request_ring(set: &Arc<PakSet>, poi: &crate::poi::PoiSet, eye: Vec3, landing: bool, built: &dyn Fn(&str) -> bool) {
    if !enabled() { return; }
    static LAST: Mutex<Option<(Vec3, std::time::Instant)>> = Mutex::new(None);
    static LAST_RESIDENT: AtomicUsize = AtomicUsize::new(usize::MAX);
    let resident = poi.groups.iter().filter(|g| g.resident.load(Ordering::Relaxed)).count();
    let ahead = {
        let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if l.is_some_and(|(p, _)| (p - eye).length() < 1000.) && LAST_RESIDENT.load(Ordering::Relaxed) == resident { return; }
        let v = l.map_or(Vec3::ZERO, |(p, t)| { let dt = t.elapsed().as_secs_f32(); if dt > 0.05 && dt < 10. { (eye - p) / dt } else { Vec3::ZERO } });
        *l = Some((eye, std::time::Instant::now()));
        LAST_RESIDENT.store(resident, Ordering::Relaxed);
        let v = if v.length() > 5000. { v * (5000. / v.length()) } else { v };
        eye + v * LOOK_AHEAD_S
    };
    let ring = crate::poi::stream_cm() * RING;
    let mut near: Vec<(f32, usize)> = poi.groups.iter().enumerate()
        .filter(|(_, g)| !g.ready.load(Ordering::Relaxed))
        .filter_map(|(i, g)| {
            let d = (g.center - eye).length() - g.radius;
            if g.resident.load(Ordering::Relaxed) { return Some((d, i)); }
            let da = (g.center - ahead).length() - g.radius;
            (d < ring || da < ring).then_some((d.min(da), i))
        }).collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    let keep_ring = if landing { crate::poi::stream_cm() * 1.2 } else { ring * KEEP };
    let keep: HashSet<&str> = poi.groups.iter()
        .filter(|g| g.resident.load(Ordering::Relaxed) || (g.center - eye).length() - g.radius < keep_ring || (g.center - ahead).length() - g.radius < keep_ring)
        .flat_map(|g| g.kinds.iter().map(|&k| poi.kinds[k as usize].id.as_str())).collect();
    let evicted = with(|s| {
        let stale: Vec<(String, bool)> = s.done.iter().filter(|k| !keep.contains(k.as_str()) && (!s.consumed.contains(*k) || s.claims.contains_key(*k))).map(|k| (k.clone(), s.consumed.contains(k))).collect();
        for (k, _) in &stale { s.done.remove(k); s.known.remove(k); s.consumed.remove(k); }
        release_all(s, stale.iter().map(|x| x.0.as_str()));
        let unbuilt = stale.iter().filter(|x| !x.1).count();
        let drop: Vec<String> = s.queued.iter().filter(|(k, &t)| t > 0 && !keep.contains(k.as_str())).map(|(k, _)| k.clone()).collect();
        if !drop.is_empty() {
            let gone: HashSet<&str> = drop.iter().map(String::as_str).collect();
            s.urgent.retain(|k| !gone.contains(k.as_str()));
            s.queue.retain(|k| !gone.contains(k.as_str()));
            for k in &drop { s.queued.remove(k); s.known.remove(k); }
        }
        unbuilt
    });
    if evicted > 0 { let _ = UNBUILT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_sub(evicted))); }
    start(set);
    let added = with(|s| {
        let mut n = 0usize;
        for &(d, gi) in &near { let urgent = poi.groups[gi].resident.load(Ordering::Relaxed) || (landing && d < crate::poi::stream_cm()); for &k in &poi.groups[gi].kinds { let id = &poi.kinds[k as usize].id; if built(id) { continue; } n += queue_in(s, id, if urgent { 1 } else { 2 }) as usize; } }
        n
    });
    if added > 0 { WAKE.notify_all(); }
}

pub(super) fn is_done(id: &str) -> bool { !enabled() || with(|s| s.done.contains(id)) }

/// Every 10 s while the main thread is waiting for kinds: what the lanes are doing (a kind that never becomes ready holds its group back).
fn status_log(waiting_for: &str) {
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    {
        let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if l.is_some_and(|t| t.elapsed().as_secs_f32() < 10.) { return; }
        *l = Some(std::time::Instant::now());
    }
    with(|s| {
        let mut slow: Vec<(f32, &String)> = s.reading.iter().map(|(k, t)| (t.elapsed().as_secs_f32(), k)).collect();
        slow.sort_by(|a, b| b.0.total_cmp(&a.0));
        let state = if s.done.contains(waiting_for) { "read" } else if s.reading.contains_key(waiting_for) { "being read" } else if s.now.iter().any(|q| q == waiting_for) { "queued (now)" } else if s.urgent.iter().any(|q| q == waiting_for) { "queued (resident)" } else if s.queue.iter().any(|q| q == waiting_for) { "queued (ring)" } else if s.known.contains(waiting_for) { "KNOWN BUT NOWHERE" } else { "not requested" };
        eprintln!("[poi prefetch] status: now {} / resident {} / ring {} queued, {} read and unbuilt (cap {AHEAD}), {} reading{}; main thread waits for {waiting_for}: {state}",
            s.now.len(), s.urgent.len(), s.queue.len(), UNBUILT.load(Ordering::Relaxed), s.reading.len(),
            slow.iter().take(3).map(|(t, k)| format!(" [{t:.0} s {}]", k.rsplit('/').next().unwrap_or(k))).collect::<String>());
    });
}
