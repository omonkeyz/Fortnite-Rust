use crate::table::{ReloadSmokeData, ShellsData};
use fn_particles::sim::{EmitterDraw, Env, Instance, Transform};
use fn_particles::{Params, ParticleSystem};
use glam::Vec3;
use std::sync::Arc;

const LOOP_EMITTERS: [(u8, &str); 4] = [(0, "Small Loop"), (1, "Medium Loop"), (2, "Large Loop"), (4, "Energy Cell Loop")];
const SINGLE_EMITTERS: [(u8, &str); 5] = [(0, "Small Single"), (1, "Medium Single"), (2, "Large Single"), (3, "Shotgun Single"), (4, "Energy Cell Single")];

pub fn setup_params(d: &ShellsData) -> Params {
    let mut p = Params::new();
    p.set_float("SpawnRateScale", d.spawn_rate_scale);
    p.set_vector("ShellsRotationRate", d.rotation_rate);
    p.set_vector("Velocity", d.velocity);
    p.set_vector("Gravity", d.gravity);
    p.set_float("Lifetime", d.lifetime);
    p.set_vector("Size", d.size);
    p.set_vector("InheritParentVelocity", [d.inherit_parent_velocity; 3]);
    p.set_float("CylinderRadius", d.cylinder_radius);
    p.set_float("CylinderHeight", d.cylinder_height);
    p
}

pub struct ShellEject {
    pub data: ShellsData,
    inst: Instance,
    active: bool,
    persistent_fire_fx: bool,
    shell_reload_counter: i32,
    socket: Transform,
    part_sys_velocity: Vec3,
    pending: Vec<(f32, bool)>,
    pub activations: u32,
}

impl ShellEject {
    pub fn new(system: Arc<ParticleSystem>, data: ShellsData, persistent_fire_fx: bool, seed: u64) -> ShellEject {
        let params = setup_params(&data);
        let mut inst = Instance::new_with_params(system, Transform::identity(), seed, 0.0, &params);
        inst.deactivate();
        ShellEject { data, inst, active: false, persistent_fire_fx, shell_reload_counter: 0, socket: Transform::identity(), part_sys_velocity: Vec3::ZERO, pending: vec![], activations: 0 }
    }
    pub fn system(&self) -> &Arc<ParticleSystem> { &self.inst.system }

    fn update_emitters(&mut self) {
        let t = self.data.shell_type;
        for (k, name) in LOOP_EMITTERS { self.inst.set_emitter_enabled(name, k == t && self.persistent_fire_fx); }
        for (k, name) in SINGLE_EMITTERS { self.inst.set_emitter_enabled(name, k == t && !self.persistent_fire_fx); }
        if self.data.on_reload {
            let burst = if self.shell_reload_counter >= 0 { self.shell_reload_counter } else { 0 };
            self.inst.set_float_parameter("BurstScale", burst as f32);
            self.shell_reload_counter = -1;
        }
    }

    fn activate_shells(&mut self, enabled: bool, camera_distance_cm: f32) -> bool {
        if !enabled { return false; }
        let lod = self.inst.system.lod_for_distance(camera_distance_cm);
        self.inst.lod = lod;
        for e in &mut self.inst.emitters { e.lod = lod; }
        self.inst.xf = self.socket;
        self.inst.reactivate();
        self.active = true;
        self.update_emitters();
        self.activations += 1;
        true
    }

    pub fn on_play_weapon_fire_fx(&mut self, using_scope_or_first_person: bool, camera_distance_cm: f32) -> bool {
        if !using_scope_or_first_person && self.activate_shells(self.data.on_fire, camera_distance_cm) { return true; }
        if self.data.on_pump { let on = self.data.on_pump; self.pending.extend(self.data.pump_notify_times.iter().map(|t| (*t, on))); }
        false
    }

    pub fn on_reload_fx(&mut self, stage: u8, camera_distance_cm: f32) -> bool {
        if stage == 0 { if let Some(t) = self.data.reload_shell_delay { self.pending.push((t, self.data.on_reload)); } }
        stage == 3 && self.activate_shells(self.data.on_reload, camera_distance_cm)
    }

    pub fn on_stop_weapon_fire_fx(&mut self) { if self.active { self.inst.deactivate(); self.active = false; } }

    pub fn cancel_pending(&mut self) { self.pending.clear(); }

    pub fn set_socket(&mut self, socket: Transform, part_sys_velocity: Vec3) { self.socket = socket; self.part_sys_velocity = part_sys_velocity; }

    pub fn tick(&mut self, dt: f32, camera_distance_cm: f32, env: &Env) {
        if !self.pending.is_empty() {
            let mut fire = None;
            for (t, on) in self.pending.iter_mut() { *t -= dt; if *t <= 0.0 { fire = Some(*on); } }
            self.pending.retain(|t| t.0 > 0.0);
            if let Some(on) = fire { self.activate_shells(on, camera_distance_cm); }
        }
        if self.inst.finished() { self.active = false; return; }
        self.inst.xf = self.socket;
        self.inst.part_sys_velocity = self.part_sys_velocity;
        self.inst.tick(dt * self.data.time_dilation, env);
    }

    pub fn draw(&self, camera_game: [f32; 3], out: &mut Vec<EmitterDraw>) { if !self.inst.finished() { self.inst.draw(camera_game, out); } }
    pub fn instance(&self) -> Option<&Instance> { if self.inst.finished() { None } else { Some(&self.inst) } }
    pub fn live_particles(&self) -> usize { self.inst.live_count() }
}

pub struct ReloadSmoke {
    pub data: ReloadSmokeData,
    inst: Instance,
    active: bool,
    socket: Transform,
}

impl ReloadSmoke {
    pub fn new(system: Arc<ParticleSystem>, data: ReloadSmokeData, seed: u64) -> ReloadSmoke {
        let mut inst = Instance::new(system, Transform::identity(), seed, 0.0);
        inst.deactivate();
        ReloadSmoke { data, inst, active: false, socket: Transform::identity() }
    }
    pub fn on_reload_fx(&mut self, stage: u8, camera_distance_cm: f32) {
        match stage {
            0 if !self.active => {
                let lod = self.inst.system.lod_for_distance(camera_distance_cm);
                self.inst.lod = lod;
                for e in &mut self.inst.emitters { e.lod = lod; }
                self.inst.xf = self.socket;
                self.inst.reactivate();
                self.active = true;
            }
            2 if self.active => { self.inst.deactivate(); self.active = false; }
            _ => {}
        }
    }
    pub fn set_socket(&mut self, socket: Transform) { self.socket = socket; }
    pub fn tick(&mut self, dt: f32, env: &Env) {
        if self.inst.finished() { self.active = false; return; }
        self.inst.xf = self.socket;
        self.inst.tick(dt, env);
    }
    pub fn instance(&self) -> Option<&Instance> { if self.inst.finished() { None } else { Some(&self.inst) } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fn_particles::dist::Dist;
    use fn_particles::model::*;

    fn system() -> Arc<ParticleSystem> {
        let em = |name: &str, loops: i32| {
            let modules: Vec<Module> = [ModuleKind::Lifetime { lifetime: Dist::constant(1.0) }, ModuleKind::Size { start: Dist::constant3([1.0; 3]) }].into_iter().map(|kind| Module { kind, enabled: true, lod_validity: 15, seed: None, name: String::new() }).collect();
            let lod = Lod { level: 0, enabled: true, required: Required { emitter_duration: 0.1, emitter_loops: loops, ..Default::default() },
                spawn: Spawn { rate: Dist::constant(0.0), rate_scale: Dist::constant(1.0), burst_scale: Dist::constant(1.0), bursts: vec![Burst { count: 1, count_low: -1, time: 0.0 }] }, type_data: TypeData::Sprite, modules, peak_active: 0 };
            Emitter { name: name.into(), lods: vec![lod] }
        };
        let emitters = LOOP_EMITTERS.iter().map(|e| em(e.1, 0)).chain(SINGLE_EMITTERS.iter().map(|e| em(e.1, 1))).collect();
        Arc::new(ParticleSystem { name: "P_S".into(), emitters, lod_distances: vec![0.0], ..Default::default() })
    }
    fn data(t: u8) -> ShellsData { ShellsData { on_fire: true, shell_type: t, spawn_rate_scale: 1.0, lifetime: 1.0, size: [1.0; 3], time_dilation: 1.0, ..Default::default() } }

    #[test] fn only_the_chosen_single_emitter_spawns_and_nothing_before_activation() {
        let env = Env::none();
        let mut s = ShellEject::new(system(), data(3), false, 5);
        for _ in 0..10 { s.tick(1.0 / 60.0, 100.0, &env); }
        assert_eq!(s.live_particles(), 0);
        assert!(s.on_play_weapon_fire_fx(false, 100.0));
        s.tick(1.0 / 60.0, 100.0, &env);
        let live: Vec<_> = s.inst.emitters.iter().filter(|e| !e.particles.is_empty()).map(|e| s.inst.system.emitters[e.index].name.clone()).collect();
        assert_eq!(live, vec!["Shotgun Single".to_string()]);
        assert!(s.on_play_weapon_fire_fx(false, 100.0));
        s.tick(1.0 / 60.0, 100.0, &env);
        assert_eq!(s.inst.emitters.iter().filter(|e| !e.particles.is_empty()).count(), 1);
        assert!(!s.on_play_weapon_fire_fx(true, 100.0), "scoped shots eject nothing");
    }
    #[test] fn persistent_fire_uses_the_loop_emitter_and_stop_deactivates() {
        let env = Env::none();
        let mut s = ShellEject::new(system(), data(0), true, 6);
        s.on_play_weapon_fire_fx(false, 100.0);
        s.tick(1.0 / 60.0, 100.0, &env);
        let live: Vec<_> = s.inst.emitters.iter().filter(|e| !e.particles.is_empty()).map(|e| s.inst.system.emitters[e.index].name.clone()).collect();
        assert_eq!(live, vec!["Small Loop".to_string()]);
        s.on_stop_weapon_fire_fx();
        let n = s.inst.spawned_total();
        for _ in 0..120 { s.tick(1.0 / 60.0, 100.0, &env); }
        assert_eq!(s.inst.spawned_total(), n);
    }
    #[test] fn pump_shells_wait_for_the_notify_time() {
        let env = Env::none();
        let mut d = data(3); d.on_fire = false; d.on_pump = true; d.pump_notify_times = vec![0.5];
        let mut s = ShellEject::new(system(), d, false, 7);
        assert!(!s.on_play_weapon_fire_fx(false, 100.0));
        for _ in 0..25 { s.tick(1.0 / 60.0, 100.0, &env); }
        assert_eq!(s.activations, 0);
        for _ in 0..10 { s.tick(1.0 / 60.0, 100.0, &env); }
        assert_eq!(s.activations, 1);
    }
    #[test] fn reload_shells_take_burst_scale_from_the_counter() {
        let mut d = data(1); d.on_fire = false; d.on_reload = true;
        let mut s = ShellEject::new(system(), d, false, 8);
        assert!(!s.on_reload_fx(0, 100.0));
        assert!(s.on_reload_fx(3, 100.0));
        assert_eq!(s.inst.params().get_float("BurstScale"), Some(0.0));
        assert_eq!(s.shell_reload_counter, -1);
    }
}
