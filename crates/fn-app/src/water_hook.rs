use glam::{Quat, Vec3};
use serde_json::Value;
use std::sync::Mutex;

pub const SHALLOW_FLUID_FRICTION: f32 = 0.5;
pub const DEEP_FLUID_FRICTION: f32 = 0.25;
pub const KILL_Z_CM: f32 = fn_movement::params::KILL_Z;
pub const KILLZ_DEATH_CAUSE: fn_ui::screens::kill_feed::DeathCause = fn_ui::screens::kill_feed::DeathCause::Unspecified;

struct Volume { level: String, t: Vec3, inv_q: Quat, s: Vec3, hulls: Vec<fn_hull::Hull>, lo: Vec3, hi: Vec3, priority: i64, friction: f32 }

static VOLUMES: Mutex<Vec<Volume>> = Mutex::new(Vec::new());

fn v3(v: &Value) -> Option<Vec3> { let a = v.as_array()?; Some(Vec3::new(a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32)) }

pub fn ingest_poi(doc: &Value) {
    crate::waterfall_fx::ingest_poi(doc);
    let level = doc["level"].as_str().unwrap_or("").to_owned();
    if let Ok(mut g) = VOLUMES.lock() { g.retain(|x| x.level != level); }
    let Some(vs) = doc["volumes"].as_array() else { return };
    let mut add = vec![];
    for v in vs {
        let friction = match v["class"].as_str() { Some("WaterVolume_Shallow") => SHALLOW_FLUID_FRICTION, Some("WaterVolume_Deep") => DEEP_FLUID_FRICTION, _ => continue };
        let (Some(t), Some(s)) = (v3(&v["t"]), v3(&v["s"])) else { continue };
        let q = v["q"].as_array().map(|a| Quat::from_xyzw(a[0].as_f64().unwrap_or(0.) as f32, a[1].as_f64().unwrap_or(0.) as f32, a[2].as_f64().unwrap_or(0.) as f32, a[3].as_f64().unwrap_or(1.) as f32)).unwrap_or(Quat::IDENTITY).normalize();
        let mut hulls = vec![];
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for b in v["boxes"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            let pts: Vec<Vec3> = b["points"].as_array().map(|a| a.iter().filter_map(v3).collect()).unwrap_or_default();
            for p in &pts { let w = t + q * (*p * s); lo = lo.min(w); hi = hi.max(w); }
            if let Some(h) = fn_hull::Hull::from_points(&pts) { hulls.push(h); }
        }
        if hulls.is_empty() { continue; }
        add.push(Volume { level: level.clone(), t, inv_q: q.inverse(), s, hulls, lo, hi, priority: v["priority"].as_i64().unwrap_or(0), friction });
    }
    if add.is_empty() { return; }
    if let Ok(mut g) = VOLUMES.lock() { eprintln!("[water] {} water volumes ({} so far)", add.len(), g.len() + add.len()); g.extend(add); }
}

pub fn fluid_friction_at(ue: Vec3) -> f32 {
    let Ok(g) = VOLUMES.lock() else { return 1. };
    let mut best: Option<(i64, f32)> = None;
    for v in g.iter() {
        if ue.cmplt(v.lo).any() || ue.cmpgt(v.hi).any() { continue; }
        let l = (v.inv_q * (ue - v.t)) / v.s;
        if v.hulls.iter().any(|h| h.sdf(l) <= 0.) && best.map_or(true, |b| v.priority > b.0) { best = Some((v.priority, v.friction)); }
    }
    best.map_or(1., |b| b.1)
}

pub fn capsule_centre_ue(feet_game: Vec3, half_height_cm: f32) -> Option<Vec3> {
    let w = fn_terrain::world()?;
    Some(w.map.to_ue(feet_game) + Vec3::Z * half_height_cm)
}

pub fn below_kill_z(centre_ue: Vec3) -> bool { centre_ue.z < KILL_Z_CM }

pub fn on_frame(sim: &mut fn_game::GameSim, arms: &mut crate::arms::Arms) {
    let hh = sim.controller.capsule_half_height_cm();
    let Some(c) = capsule_centre_ue(sim.feet(), hh) else { return };
    let fr = fluid_friction_at(c);
    let cmc = sim.controller.component_mut();
    if cmc.physics_volume_fluid_friction != fr {
        let v = cmc.velocity;
        eprintln!("[water] physics volume FluidFriction {} -> {fr} at UE ({:.0}, {:.0}, {:.0}), speed {:.0} cm/s", cmc.physics_volume_fluid_friction, c.x, c.y, c.z, (v.x * v.x + v.y * v.y).sqrt());
    }
    cmc.physics_volume_fluid_friction = fr;
    if fn_live::var("FN_WATER_DEBUG").is_ok() && sim.frames % 30 == 0 {
        let v = sim.controller.component().velocity;
        eprintln!("[water] f{} capsule centre UE ({:.0}, {:.0}, {:.0}) friction {fr} speed {:.0} cm/s max {:.0}", sim.frames, c.x, c.y, c.z, (v.x * v.x + v.y * v.y).sqrt(), sim.controller.component().get_max_speed());
    }
    if below_kill_z(c) && arms.health > 0. && sim.fly == fn_game::sim::FlyMode::Walk {
        eprintln!("[water] fell out of the world: capsule centre z {:.0} cm < KillZ {KILL_Z_CM} -> Gameplay.Effect.InstantDeath.Environment", c.z);
        arms.shield = 0.;
        arms.health = 0.;
        crate::kill_feed_hook::local_damaged(KILLZ_DEATH_CAUSE, fn_ui::screens::kill_feed::Killer::Themself, 0.);
    }
}
