use crate::data::Phoebe;
use serde_json::Value;

pub const SLOTS: [(&str, &str); 18] = [
    ("Aiming", "AI_skill_phoebe_bot_aiming"), ("Attacking", "AI_skill_phoebe_bot_attacking"), ("Building", "AI_skill_phoebe_bot_building"), ("DBNO", "AI_skill_phoebe_bot_DBNO"),
    ("Emote", "AI_skill_phoebe_bot_emote"), ("EvasiveManeuvers", "AI_skill_phoebe_bot_evasiveManeuvers"), ("Harvest", "AI_skill_phoebe_bot_harvest"), ("Healing", "AI_skill_phoebe_bot_healing"),
    ("Inventory", "AI_skill_phoebe_bot_inventory"), ("Looting", "AI_skill_phoebe_bot_looting"), ("Movement", "AI_skill_phoebe_bot_movement"), ("Perception", "AI_skill_phoebe_bot_perception"),
    ("PlayStyle", "AI_skill_phoebe_bot_playstyle"), ("RangeAttack", "AI_skill_phoebe_bot_rangeAttack"), ("Revive", "AI_skill_phoebe_bot_revive"), ("Unstuck", "AI_skill_phoebe_bot_unstuck"),
    ("Vehicle", "AI_skill_Phoebe_bot_vehicle"), ("WarmUp", "AI_skill_phoebe_bot_warmup"),
];

pub fn scalable(data: &Phoebe, v: &Value, axis: f32) -> Option<f32> {
    if let Some(x) = v.as_f64() { return Some(x as f32); }
    if let Some(b) = v.as_bool() { return Some(b as i32 as f32); }
    let mul = v.get("v").and_then(|x| x.as_f64()).unwrap_or(1.) as f32;
    let (t, r) = (v.get("t")?.as_str()?, v.get("r")?.as_str()?);
    data.row(t, r, axis).map(|c| c * mul)
}

pub fn prop<'a>(data: &'a Phoebe, set: &str, path: &str) -> Option<&'a Value> {
    let mut v = data.skills["skillsets"].get(set)?.get("p")?;
    for part in path.split('.') { v = v.get(part)?; }
    Some(v)
}

pub fn skill(data: &Phoebe, set: &str, path: &str, level: f32) -> Option<f32> { prop(data, set, path).and_then(|v| scalable(data, v, level)) }

pub fn level_range(data: &Phoebe) -> (f32, f32) {
    let lo = data.row("Phoebe_SkillSets", "Phoebe.MinELO", 0.).unwrap_or(1100.);
    let hi = data.row("Phoebe_SkillSets", "Phoebe.MaxELO", 0.).unwrap_or(2400.);
    (lo.min(hi), hi.max(lo))
}

#[derive(Clone, Debug)]
pub struct Digest {
    pub level: f32,
    pub sight_reaction: f32, pub sight_dev: f32, pub lose_sight: f32, pub lose_sight_dev: f32, pub damage_reaction: f32, pub hearing_reaction: f32, pub hearing_error_cm: f32,
    pub sight_radius: [f32; 2], pub lose_sight_radius: [f32; 2], pub half_angle: [f32; 2], pub backward_cm: [f32; 2], pub near_clip_cm: [f32; 2],
    pub alerted_loudness: f32, pub lkp_loudness: f32,
    pub tracking_reaction: f32, pub tracking_interp: f32, pub targeting_interval: f32, pub targeting_dev: f32,
    pub jump_off_angle: (f32, f32), pub patrol_distance_cm: f32, pub patrol_dev_cm: f32, pub lkp_offset_cm: (f32, f32),
    pub loot_radius_cm: f32, pub loot_dist_score: f32, pub loot_height_score: f32, pub loot_weapon_score: f32, pub loot_pickup_time: f32, pub loot_pickup_dev: f32, pub loot_min_s: f32, pub loot_max_s: f32,
    pub sprint_min_dist_cm: f32, pub glider_landing_cm: f32, pub danger_reaction: f32, pub danger_reaction_dev: f32,
    pub wobble_delay: f32, pub wobble_dev: f32,
    pub aggressiveness: f32,
}

impl Digest {
    pub fn new(data: &Phoebe, level: f32) -> Digest {
        let s = |set: &str, p: &str, d: f32| skill(data, set, p, level).unwrap_or(d);
        let r = |row: &str, d: f32| data.row("Phoebe_SkillSets", row, level).unwrap_or(d);
        let pr = "AI_skill_phoebe_bot_perception";
        let sight = |alert: &str, f: &str, d: f32| r(&format!("Phoebe.Skill.Perception.AlertLevel.{alert}.{f}"), d);
        Digest {
            level,
            sight_reaction: s(pr, "SightReactionTime", 0.15), sight_dev: s(pr, "SightRandomDeviation", 0.05), lose_sight: s(pr, "LoseSightTime", 1.), lose_sight_dev: s(pr, "LoseSightRandomDeviation", 0.2),
            damage_reaction: s(pr, "DamageReactionTime", 0.1), hearing_reaction: s(pr, "HearingReactionTime", 0.1), hearing_error_cm: s(pr, "MaxHearingLocationError", 800.),
            sight_radius: [sight("Unaware", "SightRadius", 8000.), sight("AlertedAndUp", "SightRadius", 40000.)],
            lose_sight_radius: [sight("Unaware", "LoseSightRadius", 10000.), sight("AlertedAndUp", "LoseSightRadius", 45000.)],
            half_angle: [sight("Unaware", "PeripheralVisionHalfAngle", 45.), sight("AlertedAndUp", "PeripheralVisionHalfAngle", 45.)],
            backward_cm: [sight("Unaware", "PointOfViewBackwardOffset", 250.), sight("AlertedAndUp", "PointOfViewBackwardOffset", 250.)],
            near_clip_cm: [sight("Unaware", "NearClippingRadius", 50.), sight("AlertedAndUp", "NearClippingRadius", 50.)],
            alerted_loudness: s(pr, "AlertedAccumulatedLoudnessLimit", 250.), lkp_loudness: s(pr, "LKPAccumulatedLoudnessLimit", 300.),
            tracking_reaction: s("AI_skill_phoebe_bot_aiming", "TrackingReactionTime", 0.4), tracking_interp: s("AI_skill_phoebe_bot_aiming", "TrackingInterpTime", 0.2),
            targeting_interval: s("AI_skill_phoebe_bot_aiming", "TargetingUpdateInterval", 0.5), targeting_dev: s("AI_skill_phoebe_bot_aiming", "TargetingUpdateIntervalMaxDeviation", 0.2),
            jump_off_angle: (r("Phoebe.Skill.Movement.JumpOffMinAngle", 65.), r("Phoebe.Skill.Movement.JumpOffMaxAngle", 115.)),
            patrol_distance_cm: r("Phoebe.Skill.Movement.MaxPatrolDistance", 9000.), patrol_dev_cm: r("Phoebe.Skill.Movement.MaxPatrolDistanceDeviation", 3000.),
            lkp_offset_cm: (r("Phoebe.Skill.Movement.LKPMinOffset", 1000.), r("Phoebe.Skill.Movement.LKPMaxOffset", 3000.)),
            loot_radius_cm: r("Phoebe.Skill.LootSelection.LootStateEvaluationRadius", 1500.), loot_dist_score: r("Phoebe.Skill.LootSelection.Distance2DScore", -1.), loot_height_score: r("Phoebe.Skill.LootSelection.HeightScore", -1000.),
            loot_weapon_score: r("Phoebe.Skill.LootSelection.PrioritizeWeaponScore", 3000.), loot_pickup_time: r("Phoebe.Skill.LootSelection.LootPickupInteractionTime", 0.3), loot_pickup_dev: r("Phoebe.Skill.LootSelection.LootPickupInteractionTimeDeviation", 0.1),
            loot_min_s: r("Phoebe.Skill.LootSelection.MinLootDurationPerPOI", 30.), loot_max_s: r("Phoebe.Skill.LootSelection.MaxLootDurationPerPOI", 120.),
            sprint_min_dist_cm: r("Phoebe.Skill.Movement.TacticalSprint.MinPathTargetDistance", 1500.), glider_landing_cm: r("Phoebe.Skill.Movement.GliderLandingDistance", 600.),
            danger_reaction: r("Phoebe.Skill.Movement.DangerZone.MaxReactionTime", 0.5), danger_reaction_dev: r("Phoebe.Skill.Movement.DangerZone.MaxReactionTimeDeviation", 0.25),
            wobble_delay: r("Phoebe.Skill.Movement.Wobbling.MaxDelayBetweenWobbling", 2.5), wobble_dev: r("Phoebe.Skill.Movement.Wobbling.MaxDelayBetweenWobblingDeviation", 1.),
            aggressiveness: data.row("Phoebe_GamePhaseSkillSet", "Phoebe.Skill.PlayStyle.Agressiveness", 0.).unwrap_or(75.),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WeaponClass { Rifle, Smg, Pistol, Shotgun, Sniper, RocketLauncher, GrenadeLauncher, Minigun, Melee }

impl WeaponClass {
    pub fn table(self) -> &'static str {
        match self { WeaponClass::Rifle => "Rifle", WeaponClass::Smg => "SMG", WeaponClass::Pistol => "Pistol", WeaponClass::Shotgun => "Shotgun", WeaponClass::Sniper => "Sniper", WeaponClass::RocketLauncher => "RocketLauncher", WeaponClass::GrenadeLauncher => "GrenadeLauncher", WeaponClass::Minigun => "Minigun", WeaponClass::Melee => "Rifle" }
    }
    pub fn of_row(row: &str) -> WeaponClass {
        let r = row.to_ascii_lowercase();
        if r.contains("harvest") || r.contains("pickaxe") { WeaponClass::Melee }
        else if r.contains("shotgun") { WeaponClass::Shotgun }
        else if r.contains("sniper") || r.contains("hunting") { WeaponClass::Sniper }
        else if r.contains("rocket") || r.contains("rpg") { WeaponClass::RocketLauncher }
        else if r.contains("grenadelauncher") || r.contains("launcher") { WeaponClass::GrenadeLauncher }
        else if r.contains("minigun") { WeaponClass::Minigun }
        else if r.contains("pistol") || r.contains("revolver") || r.contains("handcannon") { WeaponClass::Pistol }
        else if r.contains("smg") || r.contains("pdw") || r.contains("tactical") && r.contains("auto") { WeaponClass::Smg }
        else { WeaponClass::Rifle }
    }
    pub fn full_auto(self) -> bool { matches!(self, WeaponClass::Rifle | WeaponClass::Smg | WeaponClass::Minigun) }
}

#[derive(Clone, Copy, Debug)]
pub struct HitTiming { pub min_at_min: f32, pub max_at_min: f32, pub min_at_max: f32, pub max_at_max: f32, pub min_dist_cm: f32, pub max_dist_cm: f32, pub prevent_headshots: bool, pub force_miss: bool }

impl HitTiming {
    pub fn new(data: &Phoebe, class: WeaponClass, level: f32) -> HitTiming {
        let t = class.table();
        let table = format!("Phoebe_{t}_SkillLevelSkillSets");
        let r = |f: &str, d: f32| data.row(&table, &format!("Phoebe.Skill.{t}.{f}"), level).unwrap_or(d);
        HitTiming { min_at_min: r("MinTimeBetweenHitsAtMinRange", 2.), max_at_min: r("MaxTimeBetweenHitsAtMinRange", 4.), min_at_max: r("MinTimeBetweenHitsAtMaxRange", 3.), max_at_max: r("MaxTimeBetweenHitsAtMaxRange", 6.),
            min_dist_cm: r("MinDistanceForTimeBetweenHits", 1000.), max_dist_cm: r("MaxDistanceForTimeBetweenHits", 10000.), prevent_headshots: r("PreventHeadshots", 1.) > 0.5, force_miss: r("ForceMissEnabled", 1.) > 0.5 }
    }
    pub fn between(&self, dist_cm: f32, u: f32) -> f32 {
        let a = ((dist_cm - self.min_dist_cm) / (self.max_dist_cm - self.min_dist_cm).max(1.)).clamp(0., 1.);
        let lo = self.min_at_min + (self.min_at_max - self.min_at_min) * a;
        let hi = self.max_at_min + (self.max_at_max - self.max_at_min) * a;
        lo + (hi - lo) * u
    }
}

pub fn circle_angles(data: &Phoebe, dist_cm: f32) -> (f32, f32) {
    let t = "Phoebe_Shared_Weapon_DistanceSkillSets";
    (data.row(t, "Phoebe.Skill.Shared.MinCircleOpeningAngleHorizontalCurve", dist_cm).unwrap_or(1.), data.row(t, "Phoebe.Skill.Shared.MaxCircleOpeningAngleHorizontalCurve", dist_cm).unwrap_or(3.))
}

pub fn firing_pattern(data: &Phoebe, class: WeaponClass, dist_cm: f32) -> &'static str {
    let range = if dist_cm < 1500. { "CloseRange" } else if dist_cm < 5000. { "MediumRange" } else { "LongRange_Var1" };
    let name: &'static str = match (class, range) {
        (WeaponClass::Shotgun, "CloseRange") => "FiringPattern_Shotgun_CloseRange", (WeaponClass::Shotgun, "MediumRange") => "FiringPattern_Shotgun_MediumRange", (WeaponClass::Shotgun, _) => "FiringPattern_Shotgun_LongRange",
        (WeaponClass::Sniper, "CloseRange") => "FiringPattern_Sniper_CloseRange", (WeaponClass::Sniper, "MediumRange") => "FiringPattern_Sniper_MediumRange", (WeaponClass::Sniper, _) => "FiringPattern_Sniper_LongRange_Var1",
        (WeaponClass::RocketLauncher | WeaponClass::GrenadeLauncher, "CloseRange") => "FiringPattern_Launcher_CloseRange", (WeaponClass::RocketLauncher | WeaponClass::GrenadeLauncher, "MediumRange") => "FiringPattern_Launcher_MediumRange", (WeaponClass::RocketLauncher | WeaponClass::GrenadeLauncher, _) => "FiringPattern_Launcher_LongRange",
        (c, "CloseRange") if c.full_auto() => "FiringPattern_Weapon_FullAuto_CloseRange", (c, "MediumRange") if c.full_auto() => "FiringPattern_Weapon_FullAuto_MediumRange", (c, _) if c.full_auto() => "FiringPattern_Weapon_FullAuto_LongRange_Var1",
        (_, "CloseRange") => "FiringPattern_Weapon_SingleShot_CloseRange", (_, "MediumRange") => "FiringPattern_Weapon_SingleShot_MediumRange", _ => "FiringPattern_Weapon_SingleShot_LongRange_Var1",
    };
    let _ = data;
    name
}
