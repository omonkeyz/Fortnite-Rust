use fn_game::containers::{self as ct, ClassDef, Rng};
use fn_game::GameSim;
use glam::Vec3;
use std::sync::{Arc, Mutex, OnceLock};

pub const FLOOR_LOOT_PACKAGE: &str = "/Game/Athena/Environments/Blueprints/Tiered_Athena_FloorLoot_01";
pub const FLOOR_LOOT_CLASS: &str = "Tiered_Athena_FloorLoot_01";
pub const SPAWNS_PER_FRAME: usize = 12;
pub const TAG_BASE: u64 = 0x464c_0000_0000_0000;

pub fn enabled() -> bool { static V: OnceLock<bool> = OnceLock::new(); *V.get_or_init(|| fn_live::var("FN_FLOOR_LOOT").map_or(true, |v| v != "0")) }


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemState { Waiting, Live, Taken }

struct Item { spawner: usize, item: String, count: u32, land_ue: Vec3, state: ItemState, tag: u64 }

struct State {
    poi: Option<Arc<crate::poi::PoiSet>>,
    items: Vec<Item>,
    seed: u64,
    empty: usize,
    rolled: bool,
    rolled_at: Option<std::time::Instant>,
    live: usize,
    in_match: bool,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn to_game(u: Vec3) -> Vec3 { Vec3::new(u.x, u.z, u.y) * 0.01 }

pub fn init() {
    if !enabled() { eprintln!("[floor-loot] off (FN_FLOOR_LOOT=0)"); return; }
    let seed = fn_live::var("FN_FLOOR_LOOT_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(3, |d| d.as_nanos() as u64 ^ 0x464c));
    std::thread::spawn(|| { let _ = crate::chest_hook::data(); });
    *STATE.lock().unwrap_or_else(|p| p.into_inner()) = Some(State { poi: None, items: vec![], seed, empty: 0, rolled: false, rolled_at: None, live: 0, in_match: false });
}

fn roll(st: &mut State, poi: &crate::poi::PoiSet, d: &crate::chest_hook::Data, def: Option<&ClassDef>, gravity_cm: f32) {
    st.items.clear();
    st.empty = 0;
    let mut tag = TAG_BASE + ((st.seed & 0xffff) << 24);
    for (si, s) in poi.floor_loot.iter().enumerate() {
        let Some(group) = ct::athena_tier_group(&s.tier_group) else { st.empty += 1; continue };
        let mut rng = Rng::new(st.seed ^ (0x464c_4f4f_5200 + si as u64));
        let drops = d.tables.roll(group, &mut rng);
        if drops.is_empty() { st.empty += 1; continue; }
        for x in drops {
            let (_, b) = ct::toss_points(def, 0, s.t, s.q, Vec3::ONE, &mut rng, gravity_cm);
            tag += 1;
            st.items.push(Item { spawner: si, item: x.item, count: x.count.max(1) as u32, land_ue: b, state: ItemState::Waiting, tag });
        }
    }
    let n = poi.floor_loot.len();
    eprintln!("[floor-loot] {n} Tiered_Athena_FloorLoot_01 spawners: {} rolled empty (Loot_AthenaFloorLoot), {} items to place (seed {})", st.empty, st.items.len(), st.seed);
    st.rolled = true;
    st.rolled_at = Some(std::time::Instant::now());
}

pub fn frame(sim: &mut GameSim) {
    if !enabled() { return; }
    let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner());
    let Some(st) = g.as_mut() else { return };
    let Some(poi) = crate::poi::current() else { return };
    let in_match = sim.drop.is_some();
    if in_match && !st.in_match { st.poi = None; st.seed = st.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); }
    st.in_match = in_match;
    if !st.poi.as_ref().is_some_and(|p| Arc::ptr_eq(p, &poi)) {
        let Some(Ok(d)) = crate::chest_hook::data_ready() else { return };
        for it in st.items.iter().filter(|i| i.state == ItemState::Live) { let _ = crate::pickups_hook::remove_tagged(it.tag); }
        let gravity_cm = { let p = fn_movement::cmc::Params::default().with_tuning(&sim.tuning); (p.world_gravity_z * p.gravity_scale).abs() };
        let def = d.classes.get(FLOOR_LOOT_CLASS).cloned();
        roll(st, &poi, &d, def.as_deref(), gravity_cm);
        st.poi = Some(poi.clone());
    }
    let resident = |gi: u32| gi != crate::poi::NO_CLUSTER && poi.groups.get(gi as usize).is_some_and(|g| g.resident.load(std::sync::atomic::Ordering::Relaxed) && g.ready.load(std::sync::atomic::Ordering::Relaxed));
    let mut budget = SPAWNS_PER_FRAME;
    let mut live = 0;
    for it in st.items.iter_mut() {
        let want = resident(poi.floor_loot[it.spawner].group);
        match it.state {
            ItemState::Waiting if want && budget > 0 => {
                let p = to_game(it.land_ue);
                let floor = sim.floor_below(Vec3::new(p.x, p.y + 0.3, p.z));
                let y = if floor > p.y - 3. { floor } else { p.y };
                crate::pickups_hook::queue_rest(&it.item, it.count, Vec3::new(p.x, y + fn_game::pickups::TOUCH_HALF_HEIGHT, p.z), it.tag);
                it.state = ItemState::Live;
                budget -= 1;
            }
            ItemState::Live if !want => {
                match crate::pickups_hook::remove_tagged(it.tag) { Ok(Some(n)) => { it.count = n; it.state = ItemState::Waiting; } Ok(None) => it.state = ItemState::Taken, Err(()) => {} }
            }
            ItemState::Live => {
                if crate::pickups_hook::tag_state(it.tag) == crate::pickups_hook::TagState::Gone { it.state = ItemState::Taken; }
            }
            _ => {}
        }
        if it.state == ItemState::Live { live += 1; }
    }
    st.live = live;
    let view = fn_live::var("FNRT_FLOOR_LOOT_VIEW").ok();
    let view_at = view.as_deref().and_then(|v| v.split(':').next()?.trim().parse::<f32>().ok());
    let parts: Vec<&str> = view.as_deref().map(|v| v.split(':').collect()).unwrap_or_default();
    let at_salty = parts.get(1) == Some(&"salty");
    let nth: usize = parts.get(2).and_then(|x| x.parse().ok()).unwrap_or(0);
    let back: f32 = parts.get(3).and_then(|x| x.parse().ok()).unwrap_or(2.5);
    static VIEWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if view_at.is_some_and(|v| st.rolled_at.is_some_and(|t| t.elapsed().as_secs_f32() >= v)) && (st.live > 0 || at_salty) && !VIEWED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let f = sim.feet();
        let s0 = crate::poi::SALTY_CM;
        let key = |i: &Item| if at_salty { let u = poi.floor_loot[i.spawner].ue; (u[0] - s0[0]).hypot(u[1] - s0[1]) } else { (to_game(i.land_ue) - f).length() };
        let mut cand: Vec<&Item> = st.items.iter().filter(|i| at_salty || i.state == ItemState::Live).collect();
        cand.sort_by(|a, b| key(a).total_cmp(&key(b)));
        let near = cand.get(nth.min(cand.len().saturating_sub(1))).map(|i| to_game(i.land_ue));
        if let Some(p) = near {
            let floor = sim.floor_below(Vec3::new(p.x, p.y + 0.3, p.z));
            let from = Vec3::new(p.x - back, sim.floor_below(Vec3::new(p.x - back, floor + 1.5, p.z)), p.z);
            sim.controller.position = glam::Vec2::new(from.x, from.z - fn_game::Z_OFFSET);
            sim.controller.velocity = glam::Vec2::ZERO; sim.controller.vertical_speed = 0.;
            sim.controller.height = from.y;
            sim.x = from.x; sim.z = from.z; sim.y = fn_game::EYE + from.y;
            let d = p - from;
            sim.yaw = d.x.atan2(-d.z).to_degrees();
            eprintln!("[floor-loot] test view: pawn moved to ({:.2}, {:.2}, {:.2}) m facing the nearest floor item at ({:.2}, {:.2}, {:.2})", from.x, from.y, from.z, p.x, floor, p.z);
        }
    }
}

pub fn report() -> String {
    STATE.lock().ok().and_then(|g| g.as_ref().map(|st| if !st.rolled { String::new() } else {
        format!("floor loot: {} spawners, {} empty, {} items, {} placed now, {} taken\n", st.poi.as_ref().map_or(0, |p| p.floor_loot.len()), st.empty, st.items.len(), st.live, st.items.iter().filter(|i| i.state == ItemState::Taken).count())
    })).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_floor_loot_class_tosses_slower_and_wider_than_a_chest() {
        let Ok(set) = fn_pak::PakSet::open_install(fn_fx::default_install()) else { return };
        let d = ct::load_class_at(&set, FLOOR_LOOT_PACKAGE, "Tiered_Athena_FloorLoot_01").unwrap();
        assert_eq!(d.tier_group, "Loot_AthenaFloorLoot");
        assert_eq!(ct::toss_speed(Some(&d)), 350.);
        assert_eq!(ct::toss_cone(Some(&d)), 180.);
        let mut rng = Rng::new(5);
        for _ in 0..200 {
            let (a, b) = ct::toss_points(Some(&d), 0, Vec3::new(100., 200., 300.), glam::Quat::IDENTITY, Vec3::ONE, &mut rng, 980.);
            assert!((a - Vec3::new(100., 200., 300.)).length() < 1e-3);
            assert!((b.z - 300.).abs() < 1e-3);
            assert!(Vec3::new(b.x - a.x, b.y - a.y, 0.).length() < 350. * 350. / 980. * 1.5);
        }
    }
}
