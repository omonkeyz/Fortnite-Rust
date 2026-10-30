use glam::Vec3;

pub const TIRE_CLASSES: [&str; 4] = ["Prop_TirePile_01", "Prop_TirePile_02", "Prop_TirePile_03", "Prop_TirePile_04"];
pub const TIRE_BOUNCE_HEIGHT: f32 = 2000.;
pub const TIRE_CEILING_BOUNCE_HEIGHT: f32 = 600.;
pub const TIRE_JUMP_HEIGHT: f32 = 600.;
pub const TIRE_CEILING_JUMP_HEIGHT: f32 = 100.;
pub const TIRE_ADD_JUMP_HEIGHT: bool = false;
pub const TIRE_CEILING_TRACE_START: f32 = 200.;
pub const TIRE_CEILING_TRACE_END: f32 = 340.;
pub const TIRE_CEILING_TRACE_RADIUS: f32 = 50.;
pub const TIRE_CEILING_TRACE_HALF_HEIGHT: f32 = 100.;
pub const TIRE_SOUND: &str = "/Game/Sounds/Fort_Test_Sounds/World_Objects/Bouncing/TwangRubber1_Cue";

pub const HYDRANT_CLASS: &str = "FireHydrant__2";
pub const HYDRANT_LAUNCH_Z: f32 = 2000.;
pub const HYDRANT_SPHERE_RADIUS: f32 = 100.;
pub const HYDRANT_SPHERE_SCALE: f32 = 0.375;
pub const HYDRANT_SPHERE_Z: f32 = 78.00189;
pub const HYDRANT_EXPLODED_STATE: &str = "Exploded";
pub const HYDRANT_BROKEN_MESH: &str = "/Game/Environments/Props/Street/Meshes/StreetProp_FireHydrant08Broken.StreetProp_FireHydrant08Broken";
pub const HYDRANT_WATER_FX: &str = "/Game/Effects/Fort_Effects/World/Props/FireHydrant/P_FireHydrant_Water_01";
pub const HYDRANT_LAUNCH_SOUND: &str = "/Game/Sounds/Fort_Test_Sounds/World_Objects/Bouncing/Hydrant_Launch_Cue";
pub const HYDRANT_WATER_LOOP: &str = "/Game/Sounds/Fort_GamePlay_Sounds/Bouncers/FireHydrant_WaterStream_Cue";
pub const HYDRANT_WATER_FADE_IN: f32 = 0.5;
pub const HYDRANT_WATER_FADE_OUT: f32 = 0.2;

pub const LAUNCHED_TAG_TIME: f32 = 0.75;

pub fn tire_jump_collision_mesh(class: &str) -> Option<String> {
    let n = TIRE_CLASSES.iter().position(|c| c.eq_ignore_ascii_case(class))? + 1;
    Some(format!("/Game/Environments/Props/Rubble/Meshes/TirePiles_0{n}_JumpCollision.TirePiles_0{n}_JumpCollision"))
}

pub fn launch_character(current: Vec3, launch: Vec3, xy_override: bool, z_override: bool) -> Vec3 {
    let mut v = launch;
    if !xy_override { v.x += current.x; v.y += current.y; }
    if !z_override { v.z += current.z; }
    v
}

pub fn tire_final_height(ceiling: bool) -> f32 {
    let (bounce, jump) = if ceiling { (TIRE_CEILING_BOUNCE_HEIGHT, TIRE_CEILING_JUMP_HEIGHT) } else { (TIRE_BOUNCE_HEIGHT, TIRE_JUMP_HEIGHT) };
    if TIRE_ADD_JUMP_HEIGHT { bounce + jump } else { bounce }
}

pub fn tire_launch(tire_velocity_z: f32, ceiling: bool) -> (Vec3, bool, bool) {
    (Vec3::new(0., 0., -tire_velocity_z + tire_final_height(ceiling)), false, true)
}

pub fn hydrant_launch(pawn_velocity_z: f32) -> (Vec3, bool, bool) {
    (Vec3::new(0., 0., -pawn_velocity_z + HYDRANT_LAUNCH_Z), false, false)
}

pub fn hydrant_water_offset(class: &str) -> Vec3 {
    match class.trim_end_matches("_C") { "Hydrant01" | "Hydrant02" | "Hydrant04" => Vec3::new(0., 0., 52.), "Hydrant03" => Vec3::new(3., 0., 52.), _ => Vec3::ZERO }
}

pub fn hydrant_sphere(scale: f32) -> (Vec3, f32) {
    (Vec3::new(0., -0.0019531259, HYDRANT_SPHERE_Z) * scale, HYDRANT_SPHERE_RADIUS * HYDRANT_SPHERE_SCALE * scale)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LaunchGate { pub until: f32 }
impl LaunchGate {
    pub fn active(&self, now: f32) -> bool { now < self.until }
    pub fn apply(&mut self, now: f32) { self.until = now + LAUNCHED_TAG_TIME; }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overlap { pub inside: bool }
impl Overlap {
    pub fn update(&mut self, inside: bool) -> bool { let began = inside && !self.inside; self.inside = inside; began }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HydrantEvent { Exploded, Launch, WaterFadeOut }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hydrant { pub exploded: bool, pub can_jump: bool, pub dead: bool }
impl Hydrant {
    pub fn on_damage(&mut self) -> Option<HydrantEvent> {
        if self.dead || self.exploded { return None; }
        self.exploded = true;
        self.can_jump = true;
        Some(HydrantEvent::Exploded)
    }
    pub fn on_death(&mut self) -> Option<HydrantEvent> {
        if self.dead { return None; }
        self.dead = true;
        Some(HydrantEvent::WaterFadeOut)
    }
    pub fn on_touch(&self, launched: bool, capsule: bool) -> Option<HydrantEvent> {
        if self.dead || launched || !(self.can_jump && capsule) { return None; }
        Some(HydrantEvent::Launch)
    }
}

pub fn tire_may_launch(launched: bool, dbno: bool) -> bool { !(launched || dbno) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn launch_character_overrides_or_adds_per_axis() {
        let cur = Vec3::new(300., -200., -900.);
        assert_eq!(launch_character(cur, Vec3::new(0., 0., 2000.), false, true), Vec3::new(300., -200., 2000.));
        assert_eq!(launch_character(cur, Vec3::new(0., 0., 2900.), false, false), Vec3::new(300., -200., 2000.));
        assert_eq!(launch_character(cur, Vec3::new(10., 20., 30.), true, true), Vec3::new(10., 20., 30.));
    }
    #[test] fn tire_bounce_is_2000_or_600_under_a_ceiling() {
        let (v, xy, z) = tire_launch(0., false);
        assert_eq!((v, xy, z), (Vec3::new(0., 0., 2000.), false, true));
        assert_eq!(tire_launch(0., true).0.z, 600.);
        let falling = Vec3::new(120., 40., -1500.);
        let (v, xy, z) = tire_launch(0., false);
        assert_eq!(launch_character(falling, v, xy, z), Vec3::new(120., 40., 2000.));
    }
    #[test] fn hydrant_launch_ends_at_2000_whatever_the_fall_speed() {
        for vz in [-1800., -10., 0., 350.] {
            let (v, xy, z) = hydrant_launch(vz);
            assert!((launch_character(Vec3::new(50., 60., vz), v, xy, z) - Vec3::new(50., 60., 2000.)).length() < 1e-3);
        }
    }
    #[test] fn the_launched_tag_gates_every_launcher_for_its_duration() {
        let mut g = LaunchGate::default();
        assert!(!g.active(1.));
        g.apply(1.);
        assert!(g.active(1.5) && g.active(1.749));
        assert!(!g.active(1.75));
        assert!(!tire_may_launch(g.active(1.2), false));
        assert!(!tire_may_launch(false, true));
        assert!(tire_may_launch(false, false));
    }
    #[test] fn begin_overlap_fires_once_per_entry() {
        let mut o = Overlap::default();
        assert!(o.update(true));
        assert!(!o.update(true));
        assert!(!o.update(false));
        assert!(o.update(true));
    }
    #[test] fn the_hydrant_explodes_on_first_damage_launches_then_fades_on_death() {
        let mut h = Hydrant::default();
        assert_eq!(h.on_touch(false, true), None);
        assert_eq!(h.on_damage(), Some(HydrantEvent::Exploded));
        assert_eq!(h.on_damage(), None);
        assert_eq!(h.on_touch(false, true), Some(HydrantEvent::Launch));
        assert_eq!(h.on_touch(true, true), None);
        assert_eq!(h.on_touch(false, false), None);
        assert_eq!(h.on_death(), Some(HydrantEvent::WaterFadeOut));
        assert_eq!(h.on_touch(false, true), None);
    }
    #[test] fn the_tire_children_use_their_own_jump_collision() {
        assert_eq!(tire_jump_collision_mesh("Prop_TirePile_03").as_deref(), Some("/Game/Environments/Props/Rubble/Meshes/TirePiles_03_JumpCollision.TirePiles_03_JumpCollision"));
        assert_eq!(tire_jump_collision_mesh("Prop_Tire_single"), None);
        assert!((hydrant_sphere(1.).1 - 37.5).abs() < 1e-4);
    }
}
