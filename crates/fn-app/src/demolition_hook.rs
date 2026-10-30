use fn_audio::demolition as dm;
use fn_combat::{is_poi_proto, POI_PROTO};
use fn_game::GameSim;
use glam::{Mat4, Vec3};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    poi: Option<Arc<crate::poi::PoiSet>>,
    surface: HashMap<u32, u8>,
    children: HashMap<u32, Vec<u32>>,
    props_len: usize,
    index: HashMap<u32, usize>,
    alive: Vec<bool>,
    killed: HashSet<u32>,
    explicit: usize,
    auto: usize,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner());
    f(g.get_or_insert_with(State::default))
}

fn class_name(poi: &crate::poi::PoiSet, actor: u32) -> &str {
    let c = poi.classes.get(actor as usize).map(|s| s.as_str()).unwrap_or("");
    c.split(':').next().unwrap_or(c)
}

fn ensure_poi(st: &mut State) -> Option<Arc<crate::poi::PoiSet>> {
    let poi = crate::poi::current()?;
    if !st.poi.as_ref().is_some_and(|p| Arc::ptr_eq(p, &poi)) {
        st.surface.clear();
        for p in &poi.placements {
            if st.surface.contains_key(&p.actor) { continue; }
            let Some(k) = poi.kinds.get(p.kind) else { continue };
            let ov = k.overrides.first().and_then(|o| o.as_deref());
            st.surface.insert(p.actor, dm::surface_of(&k.mesh, ov));
        }
        st.children.clear();
        st.props_len = usize::MAX;
        st.poi = Some(poi.clone());
    }
    Some(poi)
}

pub fn poi_sound(proto: u32) -> Option<&'static fn_audio::events::PropSound> {
    if !is_poi_proto(proto) { return None; }
    let actor = proto & !POI_PROTO;
    with(|st| {
        let poi = ensure_poi(st)?;
        let class = class_name(&poi, actor);
        let resource = dm::class(class).map_or(4, |r| r.resource);
        let surface = st.surface.get(&actor).copied().unwrap_or_else(|| dm::class(class).map_or(0, |r| r.surface));
        Some(dm::prop_sound(class, surface, resource))
    })
}

pub fn cascade_killed(proto: u32) -> bool { with(|st| st.killed.contains(&proto)) }

fn to_ue(g: Vec3) -> Vec3 { Vec3::new(g.x, g.z, g.y) * 100. }

fn box_dist2(p: Vec3, lo: Vec3, hi: Vec3) -> f32 { (p.clamp(lo, hi) - p).length_squared() }

fn build_links(st: &mut State, poi: &crate::poi::PoiSet, props: &[fn_combat::Harvestable]) {
    st.children.clear();
    st.explicit = 0;
    st.auto = 0;
    let bounds: HashMap<u32, (Vec3, Vec3)> = st.index.iter().filter_map(|(&a, &i)| {
        let hs = props[i].hull.as_ref()?;
        if hs.hulls.is_empty() { return None; }
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for h in &hs.hulls { let (a, b) = (to_ue(h.min), to_ue(h.max)); lo = lo.min(a.min(b)); hi = hi.max(a.max(b)); }
        Some((a, (lo, hi)))
    }).collect();
    let mut by_key: HashMap<(&str, i64, &str), u32> = HashMap::new();
    for (a, (l, n, g)) in poi.actor_keys.iter().enumerate() { by_key.insert((&**l, *g, n.as_str()), a as u32); }
    let links: HashMap<(&str, &str), &str> = dm::ATTACH_LINKS.iter().map(|(l, c, p)| ((*l, *c), *p)).collect();
    let mut rot_of: HashMap<u32, Mat4> = HashMap::new();
    for p in &poi.placements { rot_of.entry(p.actor).or_insert(p.l2w); }
    const CELL: f32 = 800.;
    let cell = |v: Vec3| ((v.x / CELL).floor() as i32, (v.y / CELL).floor() as i32);
    let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
    for (&a, &(lo, hi)) in &bounds {
        if dm::class(class_name(poi, a)).map_or(3, |r| r.slot) == 3 { continue; }
        let (c0, c1) = (cell(lo), cell(hi));
        for x in c0.0..=c1.0 { for y in c0.1..=c1.1 { grid.entry((x, y)).or_default().push(a); } }
    }
    let mut parent_of: HashMap<u32, u32> = HashMap::new();
    let mut actors: Vec<u32> = st.index.keys().copied().collect();
    actors.sort();
    for &a in &actors {
        if let Some((l, n, g)) = poi.actor_keys.get(a as usize) {
            if let Some(p) = links.get(&(&**l, n.as_str())).and_then(|pn| by_key.get(&(&**l, *g, *pn))) {
                if *p != a && st.index.contains_key(p) { parent_of.insert(a, *p); st.explicit += 1; continue; }
            }
        }
        let Some(row) = dm::class(class_name(poi, a)) else { continue };
        if row.attach == 6 || row.grid == Some(true) { continue; }
        let Some(&(clo, chi)) = bounds.get(&a) else { continue };
        let i = st.index[&a];
        let loc = to_ue(props[i].pos);
        let q = rot_of.get(&a).map(|m| m.to_scale_rotation_translation().1).unwrap_or_default();
        let at = loc + q * Vec3::from(row.offset);
        let r = row.radius;
        let mut best: Option<(f32, u32)> = None;
        let mut seen = HashSet::new();
        for x in cell(at - Vec3::splat(r)).0..=cell(at + Vec3::splat(r)).0 {
            for y in cell(at - Vec3::splat(r)).1..=cell(at + Vec3::splat(r)).1 {
                for &c in grid.get(&(x, y)).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if c == a || !seen.insert(c) { continue; }
                    let Some(cr) = dm::class(class_name(poi, c)) else { continue };
                    if !dm::can_attach(row.attach, cr.slot) { continue; }
                    let (lo, hi) = bounds[&c];
                    if box_dist2(at, lo, hi) > r * r { continue; }
                    let ci = st.index[&c];
                    let Some(hs) = props[ci].hull.as_ref() else { continue };
                    if hs.sdf(Vec3::new(at.x, at.z, at.y) * 0.01) > r * 0.01 { continue; }
                    let center_z = (lo.z + hi.z) * 0.5;
                    let mut factor = 1.;
                    let needs_box = if center_z <= at.z { row.attach == 2 } else { row.attach == 0 || row.attach == 5 };
                    if needs_box {
                        let overlap = clo.x < hi.x && lo.x < chi.x && clo.y < hi.y && lo.y < chi.y && clo.z < hi.z && lo.z < chi.z;
                        if !overlap { continue; }
                        factor = 2.;
                    }
                    let d = box_dist2(at, lo, hi).max(1.) * factor;
                    if best.map_or(true, |b| d < b.0) { best = Some((d, c)); }
                }
            }
        }
        if let Some((_, p)) = best { parent_of.insert(a, p); st.auto += 1; }
    }
    for c in &poi.containers {
        if c.searched_actor == crate::poi::NO_CLUSTER || !st.index.contains_key(&c.searched_actor) { continue; }
        if let Some(&p) = parent_of.get(&c.actor) { parent_of.insert(c.searched_actor, p); }
    }
    for (&c, &p) in &parent_of {
        let mut q = p;
        let mut cyc = false;
        for _ in 0..64 { if q == c { cyc = true; break; } match parent_of.get(&q) { Some(&n) => q = n, None => break } }
        if !cyc { st.children.entry(p).or_default().push(c); }
    }
    for v in st.children.values_mut() { v.sort(); }
    if let Some(f) = fn_live::var("FN_DEMOLITION_LOG").ok().map(|v| v.split(',').filter_map(|x| x.trim().parse::<f32>().ok()).collect::<Vec<f32>>()).filter(|f| f.len() == 3) {
        let off = poi.off.w_axis.truncate();
        let world = |a: u32| st.index.get(&a).map(|&i| to_ue(props[i].pos) - off).unwrap_or(Vec3::ZERO);
        for (&p, kids) in &st.children {
            for &c in kids {
                let w = world(c);
                if (w.x - f[0]).hypot(w.y - f[1]) > f[2] { continue; }
                let pw = world(p);
                eprintln!("[demolition] link {} #{c} at ({:.0}, {:.0}, {:.0}) -> parent {} #{p} at ({:.0}, {:.0}, {:.0}){}", class_name(poi, c), w.x, w.y, w.z, class_name(poi, p), pw.x, pw.y, pw.z, if parent_of.get(&c) == Some(&p) && poi.actor_keys.get(c as usize).is_some_and(|(l, n, _)| links.contains_key(&(&**l, n.as_str()))) { " explicit" } else { "" });
            }
        }
    }
    eprintln!("[demolition] attachments: {} explicit (ParentActorToAttachTo), {} automatic (FUN_1404db1d0), {} parents", st.explicit, st.auto, st.children.len());
}

pub fn tick(sim: &mut GameSim) {
    with(|st| {
        let Some(poi) = ensure_poi(st) else { return };
        let props = &mut sim.world.props;
        if st.props_len != props.len() {
            st.props_len = props.len();
            st.index = props.iter().enumerate().filter(|(_, h)| is_poi_proto(h.proto)).map(|(i, h)| (h.proto & !POI_PROTO, i)).collect();
            if st.children.is_empty() { build_links(st, &poi, props); }
            st.alive = props.iter().map(|h| h.alive()).collect();
            return;
        }
        let mut queue: Vec<u32> = vec![];
        st.alive.resize(props.len(), false);
        for (h, was) in props.iter().zip(st.alive.iter_mut()) {
            let a = h.alive();
            if *was && !a && is_poi_proto(h.proto) { queue.push(h.proto & !POI_PROTO); }
            *was = a;
        }
        while let Some(a) = queue.pop() {
            let Some(kids) = st.children.get(&a).cloned() else { continue };
            for c in kids {
                let Some(&ci) = st.index.get(&c) else { continue };
                let h = &mut props[ci];
                if !h.alive() { continue; }
                h.health = 0.;
                st.alive[ci] = h.alive();
                st.killed.insert(POI_PROTO | c);
                eprintln!("[demolition] f{} cascade: {} #{} destroyed with its parent {} #{}", sim.frames, class_name(&poi, c), c, class_name(&poi, a), a);
                queue.push(c);
            }
        }
    });
}

pub fn report() -> String {
    with(|st| format!("demolition: {} explicit + {} automatic attachments, {} actors destroyed by a parent\n", st.explicit, st.auto, st.killed.len()))
}
