use super::*;
use glam::{Vec2, Vec3};

fn assets() -> std::path::PathBuf {
    let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let a = here.join("../../../FortniteRuntime/assets/bots");
    if a.exists() { a } else { std::path::PathBuf::from("D:/Fortnite/FortniteRust/FortniteRuntime/assets/bots") }
}

fn load() -> Option<Phoebes> {
    let dir = assets();
    if !dir.join(data::FILES[0]).exists() { eprintln!("bot assets missing at {}", dir.display()); return None; }
    Some(Phoebes::new(Phoebe::load_dir(&dir).expect("parse")).expect("graph"))
}

struct Mock { now: f32, phase: (i32, i32), agents: Vec<Agent>, pois: Vec<Poi>, loot: Vec<Loot>, bus: Option<Bus>, storm: Option<Storm> }

impl World for Mock {
    fn now(&self) -> f32 { self.now }
    fn game_phase(&self) -> (i32, i32) { self.phase }
    fn agents(&self) -> &[Agent] { &self.agents }
    fn los(&self, _a: Vec3, _b: Vec3) -> bool { true }
    fn pois(&self) -> &[Poi] { &self.pois }
    fn loot_near(&self, p: Vec3, r: f32, out: &mut Vec<Loot>) { out.extend(self.loot.iter().filter(|l| l.pos.distance(p) <= r).cloned()); }
    fn storm(&self) -> Option<Storm> { self.storm }
    fn bus(&self) -> Option<Bus> { self.bus }
    fn sounds(&self) -> &[Sound] { &[] }
    fn poi_occupancy(&self, _i: usize) -> u32 { 0 }
    fn find_path(&self, _a: Vec3, _b: Vec3) -> Option<Vec<Vec3>> { None }
    fn random_point(&self, _c: Vec3, _r: f32, _s: u64) -> Option<Vec3> { None }
}

fn mock(ph: &Phoebes) -> Mock {
    let e = |n: &str, v: &str| ph.data.enum_value(n, v);
    Mock { now: 0., phase: (e("EAthenaGamePhase", "Warmup"), e("EAthenaGamePhaseStep", "Warmup")), agents: vec![], pois: vec![Poi { center: Vec3::new(500., 10., 0.), radius: 60., name: "A".into() }, Poi { center: Vec3::new(-800., 5., 300.), radius: 60., name: "B".into() }], loot: vec![], bus: None, storm: None }
}

fn step(bot: &mut Bot, ph: &Phoebes, w: &mut Mock, secs: f32) {
    let dt = 0.05;
    let n = (secs / dt) as usize;
    for _ in 0..n {
        w.now += dt;
        bot.think(&ph.graph, w, dt);
        let v = bot.core.intent.move_dir * if bot.core.intent.sprint { 8. } else { 5.5 };
        if bot.core.body == Body::Ground || bot.core.body == Body::Island { bot.core.feet += Vec3::new(v.x, 0., v.y) * dt; }
    }
}

#[test]
fn enums_and_graph() {
    let Some(ph) = load() else { return };
    assert_eq!(ph.data.enum_value("EExecutionStatus", "ExecutionPending"), 3);
    assert_eq!(ph.data.enum_value("EAthenaGamePhase", "Aircraft"), 3);
    assert!(ph.graph.nodes.len() > 300, "{} nodes", ph.graph.nodes.len());
    let bb = &ph.graph.schema;
    assert_eq!(bb.keys.len(), 211);
}

#[test]
fn warmup_bus_skydive_ground() {
    let Some(ph) = load() else { return };
    let mut w = mock(&ph);
    let mut bot = ph.spawn(1000, 1, "Bot".into(), 7);
    bot.runner.trace = true;
    bot.core.body = Body::Island;
    w.loot.push(Loot { id: 5, pos: Vec3::new(4., 0., 0.), kind: 0, weapon: true });
    step(&mut bot, &ph, &mut w, 3.);
    let log = bot.runner.log.join("\n");
    assert!(log.contains("BT_Phoebe_Warmup"), "warmup not entered:\n{log}");
    assert!(bot.core.intent.interact == Some(5) || bot.core.stats.loot_interactions > 0 || bot.core.feet.x > 1., "no loot move: {:?}\n{log}", bot.core.feet);
    let e = |n: &str, v: &str| ph.data.enum_value(n, v);
    w.phase = (e("EAthenaGamePhase", "Aircraft"), e("EAthenaGamePhaseStep", "BusFlying"));
    bot.core.body = Body::Bus;
    bot.runner.log.clear();
    w.bus = Some(Bus { pos: Vec3::new(-2000., 800., 0.), dir: Vec3::X, can_jump: true, time_left: 60. });
    let mut exited = false;
    for _ in 0..800 {
        step(&mut bot, &ph, &mut w, 0.05);
        if let Some(b) = w.bus.as_mut() { b.pos += Vec3::X * 0.05 * 60.; bot.core.feet = b.pos; }
        if bot.core.intent.exit_bus { exited = true; break; }
    }
    let log = bot.runner.log.join("\n");
    assert!(exited, "never jumped off the bus: bus at {:?}\n{}", w.bus.map(|b| b.pos), &log[log.len().saturating_sub(3000)..]);
    bot.core.body = Body::Skydive;
    bot.core.stats.jumped_at = Some(w.now);
    w.phase = (e("EAthenaGamePhase", "SafeZones"), e("EAthenaGamePhaseStep", "StormForming"));
    step(&mut bot, &ph, &mut w, 1.);
    assert!(bot.core.intent.aerial.length() > 0.5 || bot.core.intent.aerial_dive, "no dive steering {:?}", bot.core.intent.aerial);
    bot.core.body = Body::Ground;
    bot.core.feet = Vec3::new(0., 0., 0.);
    step(&mut bot, &ph, &mut w, 2.);
    assert!(bot.core.bb.boolean("AIEvaluator_Global_HasEverJumpedFromBusAndLandedKey"));
    let before = bot.core.feet;
    step(&mut bot, &ph, &mut w, 4.);
    assert!(bot.core.feet.distance(before) > 3., "bot did not travel toward a POI on the ground: {:?}\n{}", bot.core.feet, bot.runner.log.iter().rev().take(40).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn fights_a_visible_enemy() {
    let Some(ph) = load() else { return };
    let mut w = mock(&ph);
    let e = |n: &str, v: &str| ph.data.enum_value(n, v);
    w.phase = (e("EAthenaGamePhase", "SafeZones"), e("EAthenaGamePhaseStep", "StormHolding"));
    let mut bot = ph.spawn(1000, 1, "Bot".into(), 3);
    bot.core.body = Body::Ground;
    bot.core.stats.jumped_at = Some(0.);
    bot.runner.trace = true;
    bot.core.weapons.push(Weapon { row: "WID_Assault_Auto_Athena_C_Ore_T02".into(), class: skill::WeaponClass::Rifle, clip: 30, ammo: 30, fire_interval: 0.18, reload_time: 2.2, range_m: 60. });
    w.agents.push(Agent { id: 1, team: 2, feet: Vec3::new(25., 0., 0.), alive: true, health: 100., ..Default::default() });
    let mut shots = 0;
    let mut hits = 0;
    for i in 0..400 { step(&mut bot, &ph, &mut w, 0.05); if i % 40 == 0 { eprintln!("t{i} alert {} tgt {:?} feet {:?} yaw {} eq {:?} rs {} ready {} fire {} pat {:?}", bot.core.bb.int("AIEvaluator_Global_AlertLevel"), bot.core.target, bot.core.feet, bot.core.yaw, bot.core.equipped, bot.core.bb.int("AIEvaluator_RangeAttack_ExecutionStatus"), bot.core.bb.boolean("AIEvaluator_RangeAttack_IsReadyToFire"), bot.core.bb.boolean("AIEvaluator_ManageWeapon_Fire"), ph.data.row("FiringPattern_Weapon_FullAuto_MediumRange", "", 1.)); } if let Some(f) = bot.core.intent.fire { shots += 1; if f.hit { hits += 1; } } }
    eprintln!("{}", bot.runner.log.iter().take(80).cloned().collect::<Vec<_>>().join("
"));
    assert_eq!(bot.core.bb.int("AIEvaluator_Global_AlertLevel"), brain::ALERT_THREATENED);
    assert!(shots > 10, "shots {shots} {:?}", bot.runner.log.iter().rev().take(20).collect::<Vec<_>>());
    assert!(hits >= 1 && hits < shots, "hits {hits}/{shots}");
    let _ = Vec2::ZERO;
}

#[test]
fn node_report_lists_every_class() {
    let Some(ph) = load() else { return };
    let r = ph.node_report();
    assert!(r.iter().any(|x| x.0 == "FortAthenaBTTask_BotMoveTo"));
    for (c, o, _, n) in &r { eprintln!("{c:60} {o:?} x{n}"); }
}
