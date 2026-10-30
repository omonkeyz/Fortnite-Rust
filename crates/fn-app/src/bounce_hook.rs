use crate::pickups_dxbc::Dxbc;
use fn_audio_sink::SoundEvent;
use fn_fx::view::ViewParams;
use fn_game::bounce::{self as b, Hydrant, HydrantEvent, LaunchGate, Overlap};
use fn_game::GameSim;
use fn_hull::Hull;
use fn_particles::assets::Catalog;
use fn_particles::{Env, Instance, Transform};
use fn_render::fx::{FxFrame, FxRenderer};
use glam::{Mat3, Vec3};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub const LOOP_KEY_BASE: u32 = 0x5800_0000;

fn trace_on() -> bool { fn_live::var("FN_BOUNCE_TRACE").is_ok_and(|v| v == "1") }
pub fn enabled() -> bool { static V: OnceLock<bool> = OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_BOUNCE").map_or(true, |v| v != "0")) }

struct Tire { actor: u32, class: String, hulls: Vec<Hull>, root: Vec3, ov: Overlap, prop: Option<usize> }
struct Hyd { actor: u32, broken: u32, root: Vec3, rot: Mat3, scale: f32, water: Vec3, st: Hydrant, ov: Overlap, prop: Option<usize>, fx: Option<Instance>, loop_on: bool }

#[derive(Default)]
struct State {
    poi: Option<Arc<crate::poi::PoiSet>>,
    tires: Vec<Tire>,
    hyds: Vec<Hyd>,
    props_len: usize,
    clock: f32,
    gate: LaunchGate,
    catalog: Option<Arc<Catalog>>,
    catalog_tried: bool,
    roots: Vec<PathBuf>,
    install: Option<PathBuf>,
    dxbc: Option<Result<Dxbc, String>>,
    serial: u64,
    log: Vec<String>,
    errors: HashSet<String>,
    stats: (u32, u32, u32),
    trace_at: f32,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
static HIDDEN: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static CONVEX: OnceLock<HashMap<String, Vec<Vec<Vec3>>>> = OnceLock::new();

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); f(g.get_or_insert_with(State::default)) }
fn say(s: &mut State, line: String) { eprintln!("[bounce] {line}"); s.log.push(line); if s.log.len() > 300 { s.log.drain(..100); } }
fn to_ue(g: Vec3) -> Vec3 { Vec3::new(g.x, g.z, g.y) * 100. }
fn to_game(u: Vec3) -> Vec3 { Vec3::new(u.x, u.z, u.y) * 0.01 }

pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) {
    with(|s| { s.install = install; s.roots = roots.to_vec(); });
    if enabled() { std::thread::spawn(|| { let _ = convexes(); }); }
}

fn convexes() -> &'static HashMap<String, Vec<Vec<Vec3>>> {
    CONVEX.get_or_init(|| {
        let mut m = HashMap::new();
        let set = match fn_game::cosmetic_swap::paks() { Ok(s) => s, Err(e) => { eprintln!("[bounce] no paks ({e}): tire piles do not bounce"); return m } };
        for c in b::TIRE_CLASSES {
            let Some(mesh) = b::tire_jump_collision_mesh(c) else { continue };
            match fn_building::pieces::load_convexes(&set, &mesh) { Ok(v) => { m.insert(c.to_owned(), v); } Err(e) => eprintln!("[bounce] {c}: {e}") }
        }
        eprintln!("[bounce] Jump_Collision hulls of {} tire pile classes", m.len());
        m
    })
}

pub fn launched() -> bool { with(|s| s.gate.active(s.clock)) }
pub fn mark_launched() { with(|s| { let now = s.clock; s.gate.apply(now); }); }
pub fn hidden_actors() -> Vec<u32> { HIDDEN.lock().map(|h| h.clone()).unwrap_or_default() }

pub fn add_broken(set: &mut crate::poi::PoiSet) {
    if !enabled() { return; }
    let mut group_of: HashMap<u32, usize> = HashMap::new();
    for (gi, g) in set.groups.iter().enumerate() { for &pi in &g.placements { group_of.entry(set.placements[pi as usize].actor).or_insert(gi); } }
    let mut added = 0;
    let mut hidden = vec![];
    let broken: Vec<crate::poi::PoiHidden> = set.hidden.iter().filter(|h| h.comp == "Broken_Hydrant" && set.is_a(h.actor, b::HYDRANT_CLASS)).cloned().collect();
    for h in broken {
        let actor = h.actor;
        let Some(&gi) = group_of.get(&actor) else { continue };
        let id = format!("poi:{}|{}", h.mesh, h.overrides.iter().map(|o| o.as_deref().unwrap_or("")).collect::<Vec<_>>().join(","));
        let k = match set.kind_index.get(&id) { Some(&k) => k, None => { set.kinds.push(crate::poi::PoiKind { id: id.clone(), mesh: h.mesh.clone(), overrides: h.overrides.clone(), vcol: None, mid0: None }); set.kind_index.insert(id, set.kinds.len() - 1); set.kinds.len() - 1 } };
        let pseudo = set.classes.len() as u32;
        set.classes.push(format!("{}:Broken_Hydrant:{actor}", b::HYDRANT_CLASS));
        let npi = set.placements.len() as u32;
        set.placements.push(crate::poi::PoiPlacement { kind: k, class: format!("{}:Broken_Hydrant", b::HYDRANT_CLASS), l2w: h.l2w, actor: pseudo, cull_cm: h.cull_cm, cast_shadow: h.cast_shadow, cluster: crate::poi::NO_CLUSTER });
        let g = &mut set.groups[gi];
        g.placements.push(npi);
        if !g.kinds.contains(&(k as u32)) { g.kinds.push(k as u32); g.kinds.sort(); }
        hidden.push(pseudo);
        added += 1;
    }
    if let Ok(mut h) = HIDDEN.lock() { *h = hidden; }
    eprintln!("[bounce] {added} Broken_Hydrant placements added (hidden until the hydrant is damaged)");
}

fn sync(s: &mut State, sim: &GameSim) {
    let Some(poi) = crate::poi::current() else { return };
    if !s.poi.as_ref().is_some_and(|p| Arc::ptr_eq(p, &poi)) {
        let conv = convexes();
        let mut tires = vec![];
        let mut hyds = vec![];
        let mut broken_of: HashMap<u32, u32> = HashMap::new();
        for (i, c) in poi.classes.iter().enumerate() {
            if let Some(rest) = c.strip_prefix(&format!("{}:Broken_Hydrant:", b::HYDRANT_CLASS)) { if let Ok(a) = rest.parse::<u32>() { broken_of.insert(a, i as u32); } }
        }
        let mut seen = HashSet::new();
        for p in &poi.placements {
            let Some(class) = poi.classes.get(p.actor as usize) else { continue };
            if !seen.insert(p.actor) { continue; }
            if let Some(hulls) = conv.get(class.as_str()) {
                let l2w = p.l2w;
                let hs: Vec<Hull> = hulls.iter().filter_map(|pts| Hull::from_points(&pts.iter().map(|v| l2w.transform_point3(*v)).collect::<Vec<_>>())).collect();
                tires.push(Tire { actor: p.actor, class: class.clone(), hulls: hs, root: l2w.w_axis.truncate(), ov: Overlap::default(), prop: None });
            } else if poi.is_a(p.actor, b::HYDRANT_CLASS) && !poi.kinds[p.kind].mesh.contains("Broken") {
                let (sc, q, t) = p.l2w.to_scale_rotation_translation();
                hyds.push(Hyd { actor: p.actor, broken: broken_of.get(&p.actor).copied().unwrap_or(u32::MAX), root: t, rot: Mat3::from_quat(q), scale: sc.x, water: b::hydrant_water_offset(class), st: Hydrant::default(), ov: Overlap::default(), prop: None, fx: None, loop_on: false });
            }
        }
        let at = |u: Vec3| { let g = to_game(u); format!("({:.2}, {:.2}, {:.2})", g.x, g.y, g.z) };
        let line = format!("{} bouncing tire piles ({} classes with hulls), {} fire hydrants; tires at {}; hydrants at {}", tires.len(), conv.len(), hyds.len(),
            tires.iter().take(12).map(|t| format!("{} {}", t.class, at(t.root))).collect::<Vec<_>>().join(", "), hyds.iter().map(|h| at(h.root)).collect::<Vec<_>>().join(", "));
        s.tires = tires; s.hyds = hyds; s.poi = Some(poi); s.props_len = usize::MAX;
        say(s, line);
    }
    if s.props_len != sim.world.props.len() {
        let by: HashMap<u32, usize> = sim.world.props.iter().enumerate().filter(|(_, h)| fn_combat::is_poi_proto(h.proto)).map(|(i, h)| (h.proto, i)).collect();
        for t in &mut s.tires { t.prop = by.get(&(fn_combat::POI_PROTO | t.actor)).copied(); }
        for h in &mut s.hyds { h.prop = by.get(&(fn_combat::POI_PROTO | h.actor)).copied(); }
        s.props_len = sim.world.props.len();
    }
}

fn capsule_hits_hull(h: &Hull, a: Vec3, b: Vec3, r: f32) -> bool {
    if h.aabb_gap(a, b) > r { return false; }
    match h.segment_nearest_param(a, b) { None => true, Some(t) => h.sdf(a + (b - a) * t) < r }
}

fn vertical_capsules_overlap(c0: Vec3, r0: f32, z0: (f32, f32), c1: Vec3, r1: f32, z1: (f32, f32)) -> bool {
    let dxy = glam::Vec2::new(c0.x - c1.x, c0.y - c1.y).length();
    let gap = (z1.0 - z0.1).max(z0.0 - z1.1).max(0.);
    (dxy * dxy + gap * gap).sqrt() < r0 + r1
}

fn tire_ceiling(root: Vec3, others: &[Vec3], pawn_r: f32, pawn_hh: f32) -> bool {
    let (r, hh) = (b::TIRE_CEILING_TRACE_RADIUS, b::TIRE_CEILING_TRACE_HALF_HEIGHT);
    let z0 = (root.z + b::TIRE_CEILING_TRACE_START - (hh - r), root.z + b::TIRE_CEILING_TRACE_END + (hh - r));
    others.iter().any(|f| { let c = *f + Vec3::new(0., 0., pawn_hh); vertical_capsules_overlap(root, r, z0, c, pawn_r, (c.z - (pawn_hh - pawn_r), c.z + (pawn_hh - pawn_r))) })
}

fn cue(s: &mut State, path: &str, at_ue: Vec3, out: &mut Vec<SoundEvent>) {
    match fn_audio_sink::bounce_cue(path) { Some(c) => out.push(SoundEvent::BuildingCue { cue: c, pos: Some(to_game(at_ue)) }), None => { if s.errors.insert(path.to_owned()) { say(s, format!("cue not in BOUNCE_CUES: {path}")); } } }
}

fn load_catalog(s: &mut State) {
    if s.catalog_tried { return; }
    s.catalog_tried = true;
    let dir = fn_live::var_os("FN_BOUNCE_FX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| s.roots.iter().map(|r| r.join("particles-bounce")).find(|d| d.join("index.json").is_file()));
    match dir.map(|d| (Catalog::load(&d), d)) {
        Some((Ok(c), d)) => { say(s, format!("particles loaded from {}", d.display())); s.catalog = Some(Arc::new(c)); }
        Some((Err(e), _)) => say(s, format!("{e}: no hydrant water")),
        None => say(s, "no `particles-bounce` folder in the asset roots: no hydrant water".into()),
    }
}

fn start_water(s: &mut State, k: usize) {
    load_catalog(s);
    let Some(cat) = s.catalog.clone() else { return };
    let Some(sys) = cat.system(b::HYDRANT_WATER_FX) else { if s.errors.insert(b::HYDRANT_WATER_FX.into()) { say(s, format!("{} is not in the particles-bounce catalog", b::HYDRANT_WATER_FX)); } return };
    s.serial += 1;
    let h = &mut s.hyds[k];
    h.fx = Some(Instance::new(sys, Transform { pos: h.root + h.rot * (h.water * h.scale), rot: h.rot, scale: h.scale }, 0x4859_4452 ^ s.serial, 0.));
}

pub fn frame(sim: &mut GameSim, dt: f32) -> Vec<SoundEvent> {
    if !enabled() { return vec![]; }
    let mut out = vec![];
    let mut launch: Option<(Vec3, bool, bool, String)> = None;
    with(|s| {
        s.clock += dt;
        sync(s, sim);
        if s.tires.is_empty() && s.hyds.is_empty() { return; }
        let comp = sim.controller.component();
        let (pr, phh) = (comp.p.radius, comp.half_height);
        let center = comp.loc + Vec3::new(0., fn_game::sim::Z_OFFSET * 100., 0.);
        let (sa, sb) = (center - Vec3::new(0., 0., (phh - pr).max(0.)), center + Vec3::new(0., 0., (phh - pr).max(0.)));
        let alive = |p: Option<usize>| p.and_then(|i| sim.world.props.get(i)).map_or(true, |h| h.alive());
        let others: Vec<Vec3> = crate::mp_hook::remote_feet().into_iter().map(to_ue).collect();
        let vz = sim.controller.velocity_cm().z;
        let gated = s.gate.active(s.clock);
        for k in 0..s.tires.len() {
            if !alive(s.tires[k].prop) { s.tires[k].ov.update(false); continue; }
            let near = (s.tires[k].root - center).truncate().length() < 600.;
            let inside = near && s.tires[k].hulls.iter().any(|h| capsule_hits_hull(h, sa, sb, pr));
            if near && trace_on() && s.clock - s.trace_at > 0.5 { s.trace_at = s.clock; let d = s.tires[k].hulls.iter().map(|h| match h.segment_nearest_param(sa, sb) { None => -1., Some(t) => h.sdf(sa + (sb - sa) * t) }).fold(f32::MAX, f32::min); let line = format!("trace {} actor {}: capsule axis ({:.0}, {:.0}, {:.0})..{:.0} r {pr:.1}, nearest Jump_Collision {d:.1} cm, inside {inside}", s.tires[k].class, s.tires[k].actor, sa.x, sa.y, sa.z, sb.z); say(s, line); }
            if !s.tires[k].ov.update(inside) { continue; }
            if !b::tire_may_launch(gated || launch.is_some(), sim.dbno) { continue; }
            let ceiling = tire_ceiling(s.tires[k].root, &others, pr, phh);
            let (v, xy, z) = b::tire_launch(0., ceiling);
            let line = format!("{} (actor {}) bounced the pawn: LaunchCharacter(0, 0, {:.0}) Z override, ceiling {ceiling}", s.tires[k].class, s.tires[k].actor, v.z);
            s.stats.0 += 1;
            say(s, line);
            cue(s, b::TIRE_SOUND, center, &mut out);
            launch = Some((v, xy, z, String::new()));
        }
        for k in 0..s.hyds.len() {
            let p = s.hyds[k].prop.and_then(|i| sim.world.props.get(i));
            let (hp, max) = p.map_or((1., 1.), |h| (h.health, h.max_hp));
            if hp <= 0. {
                if s.hyds[k].st.on_death() == Some(HydrantEvent::WaterFadeOut) {
                    if s.hyds[k].loop_on { out.push(SoundEvent::ContainerLoopStop { key: LOOP_KEY_BASE + k as u32, fade: b::HYDRANT_WATER_FADE_OUT }); s.hyds[k].loop_on = false; }
                    if let Some(f) = s.hyds[k].fx.as_mut() { f.deactivate(); }
                    s.hyds[k].fx = None;
                    let line = format!("fire hydrant (actor {}) destroyed: water loop FadeOut({})", s.hyds[k].actor, b::HYDRANT_WATER_FADE_OUT);
                    say(s, line);
                }
                continue;
            }
            if hp < max && s.hyds[k].st.on_damage() == Some(HydrantEvent::Exploded) {
                s.stats.1 += 1;
                start_water(s, k);
                if let Some(c) = fn_audio_sink::bounce_cue(b::HYDRANT_WATER_LOOP) { out.push(SoundEvent::ContainerLoopFadeIn { cue: c, key: LOOP_KEY_BASE + k as u32, pos: Some(to_game(s.hyds[k].root)), fade: b::HYDRANT_WATER_FADE_IN }); s.hyds[k].loop_on = true; }
                let line = format!("fire hydrant (actor {}) damaged ({hp:.0} / {max:.0}): SetCustomState(Exploded) -> broken mesh, water on, Can_Jump", s.hyds[k].actor);
                say(s, line);
            }
            let (off, r) = b::hydrant_sphere(s.hyds[k].scale);
            let sc = s.hyds[k].root + s.hyds[k].rot * off;
            let near = (sc - center).length() < 400.;
            let inside = near && { let ab = sb - sa; let t = if ab.length_squared() > 0. { ((sc - sa).dot(ab) / ab.length_squared()).clamp(0., 1.) } else { 0. }; (sa + ab * t - sc).length() < r + pr };
            if !s.hyds[k].ov.update(inside) { continue; }
            if s.hyds[k].st.on_touch(gated || launch.is_some(), true) != Some(HydrantEvent::Launch) { continue; }
            let (v, xy, z) = b::hydrant_launch(vz);
            s.stats.2 += 1;
            let line = format!("fire hydrant (actor {}) launched the pawn: LaunchCharacter(0, 0, {:.0}) additive (Vz {vz:.0} -> 2000)", s.hyds[k].actor, v.z);
            say(s, line);
            start_water(s, k);
            cue(s, b::HYDRANT_LAUNCH_SOUND, s.hyds[k].root, &mut out);
            launch = Some((v, xy, z, String::new()));
        }
        let mut hidden = vec![];
        for h in &s.hyds {
            let alive_now = !h.st.dead;
            if h.st.exploded && alive_now { hidden.push(h.actor); }
            if h.broken != u32::MAX && !(h.st.exploded && alive_now) { hidden.push(h.broken); }
        }
        if let Ok(mut g) = HIDDEN.lock() { *g = hidden; }
        if launch.is_some() { let now = s.clock; s.gate.apply(now); }
    });
    if let Some((v, xy, z, _)) = launch {
        let fin = sim.controller.launch(v, xy, z);
        sim.fall.ignore_next = true;
        with(|s| say(s, format!("pawn launch velocity ({:.0}, {:.0}, {:.0}) cm/s, bIgnoreNextFallingDamage", fin.x, fin.y, fin.z)));
    }
    out
}

fn dxbc<'a>(slot: &'a mut Option<Result<Dxbc, String>>, install: &Option<PathBuf>, fx: &mut FxRenderer) -> Option<&'a mut Dxbc> {
    if slot.is_none() {
        let r = match install { Some(d) => Dxbc::load(d, fx), None => Err("no install".into()) };
        if let Err(e) = &r { eprintln!("[bounce] hydrant water materials unavailable: {e}"); }
        *slot = Some(r);
    }
    slot.as_mut().and_then(|r| r.as_mut().ok())
}

pub fn fx_frame(fx: &mut FxRenderer, view: &ViewParams, dt: f32, out: &mut FxFrame) {
    with(|st| {
        let Some(cat) = st.catalog.clone() else { return };
        let env = Env::none();
        let mut any = false;
        for h in st.hyds.iter_mut() { if let Some(i) = h.fx.as_mut() { i.tick(dt, &env); any = true; } }
        if !any { return; }
        let eye_ue = fn_fx::view::to_ue(view.eye).as_vec3();
        let eye = [eye_ue.x, eye_ue.y, eye_ue.z];
        let State { dxbc: slot, install, hyds, errors, .. } = st;
        let Some(dx) = dxbc(slot, install, fx) else { return };
        dx.begin(view);
        for inst in hyds.iter().filter_map(|h| h.fx.as_ref()) {
            let mut d = vec![];
            inst.draw(eye, &mut d);
            let bounds = crate::particles::instance_bounds(inst, &cat);
            for e in &d {
                if let Err(err) = dx.draw_emitter(fx, e, &inst.xf, &bounds, dt) { let k = format!("{}/{}", e.system, e.emitter); if errors.insert(k.clone()) { eprintln!("[bounce] {k}: NOT DRAWN: {err}"); } }
            }
        }
        let fr = dx.take_frame();
        out.buffer_writes.extend(fr.buffer_writes);
        out.draws.extend(fr.draws);
    });
}

pub fn report() -> String {
    with(|s| {
        if s.log.is_empty() { return String::new(); }
        let mut r = format!("bounce: {} tire piles, {} hydrants; {} tire bounces, {} hydrants broken, {} hydrant launches\n", s.tires.len(), s.hyds.len(), s.stats.0, s.stats.1, s.stats.2);
        for l in s.log.iter().rev().take(40).collect::<Vec<_>>().into_iter().rev() { r.push_str("  "); r.push_str(l); r.push('\n'); }
        r
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn a_pawn_above_the_tire_is_the_ceiling() {
        let root = Vec3::new(0., 0., 1000.);
        assert!(!tire_ceiling(root, &[], 34., 90.));
        assert!(tire_ceiling(root, &[Vec3::new(20., 0., 1250.)], 34., 90.));
        assert!(!tire_ceiling(root, &[Vec3::new(200., 0., 1250.)], 34., 90.));
        assert!(!tire_ceiling(root, &[Vec3::new(0., 0., 1700.)], 34., 90.));
    }
    #[test] fn the_capsule_overlaps_a_hull_it_touches() {
        let h = Hull::from_points(&[Vec3::new(-50., -50., 0.), Vec3::new(50., -50., 0.), Vec3::new(-50., 50., 0.), Vec3::new(50., 50., 0.), Vec3::new(-50., -50., 80.), Vec3::new(50., -50., 80.), Vec3::new(-50., 50., 80.), Vec3::new(50., 50., 80.)]).unwrap();
        assert!(capsule_hits_hull(&h, Vec3::new(0., 0., 110.), Vec3::new(0., 0., 230.), 34.));
        assert!(!capsule_hits_hull(&h, Vec3::new(0., 0., 120.), Vec3::new(0., 0., 240.), 34.));
        assert!(capsule_hits_hull(&h, Vec3::new(80., 0., 40.), Vec3::new(80., 0., 160.), 34.));
    }
}
