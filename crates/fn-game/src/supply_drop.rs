use crate::containers::{curve_row, eval_keys};
use crate::storm_zones::{self, CrtRand, PHASE_COUNT, SAFE_ZONE_DEFS};
use fn_pak::PakSet;

pub const GAME_DATA: &str = "/Game/Athena/Balance/DataTables/AthenaGameData";
pub const BUILDING_PROPS: &str = "/Game/Balance/DataTables/AttributesBuildingProps";
pub const SUPPLY_DROP_CLASS: &str = "/Game/Athena/SupplyDrops/AthenaSupplyDrop_02";
pub const LOOT_GROUP: &str = "Loot_AthenaSupplyDrop";
pub const PICK_TRIES: u32 = 1000;
pub const GROUND_TRACE_CM: f32 = 50_000.;
pub const LOOT_RADIUS_CM: (f32, f32) = (20., 45.);
pub const LOOT_SPAWN_OFFSET_CM: [f32; 3] = [0., 0., 60.];
pub const POPPED_PLAY_RATE: f32 = 2.;
pub const DISABLE_FLARE_T: f32 = 0.7;
pub const ALMOST_LANDED_T: f32 = 0.95;
pub const LAND_FX_DELAY: f32 = 0.2;
pub const FLARE_FADE_OUT: f32 = 3.;
pub const DROP_AUDIO_FADE_IN: f32 = 3.;
pub const FALLING_AUDIO_FADE_IN: f32 = 1.;
pub const BALLOON_SCALE: [f32; 3] = [0.9 * 0.9, 0.9 * 0.9, 0.9];
pub const LERP_FALL_RATE: [(f32, f32, f32, f32); 2] = [(0., 0., 0., 0.), (1., 1., 2.476_344_8, 0.)];

#[derive(Clone, Debug, PartialEq)]
pub struct Data {
    pub interval: f32,
    pub deviation: f32,
    pub fall_time: f32,
    pub fall_height: f32,
    pub min_placement: f32,
    pub max_placement: f32,
    pub counts: [(i32, i32); PHASE_COUNT],
    pub balloon_health: f32,
}

impl Default for Data {
    fn default() -> Self {
        Data { interval: 180., deviation: 30., fall_time: 60., fall_height: 10_000., min_placement: -5_000., max_placement: 10_000., counts: [(2, 2), (0, 2), (0, 2), (0, 2), (0, 2), (0, 1), (0, 1), (0, 0), (0, 0), (0, 0)], balloon_health: 1250. }
    }
}

impl Data {
    pub fn load(set: &PakSet) -> Result<Data, String> {
        let r = |row: &str| -> Result<f32, String> { Ok(eval_keys(&curve_row(set, GAME_DATA, row)?, 0.)) };
        let mut counts = [(0, 0); PHASE_COUNT];
        for (i, c) in counts.iter_mut().enumerate() { *c = (r(&format!("Default.SafeZone.{i:02}.SupplyDropMinCount"))? as i32, r(&format!("Default.SafeZone.{i:02}.SupplyDropMaxCount"))? as i32); }
        Ok(Data {
            interval: r("Default.SupplyDrop.TimeInterval")?,
            deviation: r("Default.SupplyDrop.TimeDeviation")?,
            fall_time: r("Default.SupplyDrop.FallTime")?,
            fall_height: r("Default.SupplyDrop.MaxPlacementHeight")?,
            min_placement: r("Default.SupplyDrop.MinPlacementHeight")?,
            max_placement: r("Default.SupplyDrop.MaxPlacementHeight")?,
            counts,
            balloon_health: eval_keys(&curve_row(set, BUILDING_PROPS, "AthenaSupplyDrop.FortHealthSet.MaxHealth")?, 0.),
        })
    }
    pub fn next_delay(&self, rng: &mut CrtRand) -> f32 { (rng.rand() as f32 * 3.051_851e-5 * (self.deviation + self.deviation) - self.deviation + self.interval).max(0.) }
    pub fn count(&self, phase: usize, rng: &mut CrtRand) -> i32 {
        let Some(&(lo, hi)) = self.counts.get(phase) else { return 0 };
        let span = hi - lo;
        if span + 1 < 1 { return lo; }
        let k = (rng.rand() as f32 * 3.051_851e-5 * (span + 1) as f32) as i32;
        lo + k.min(span)
    }
}

pub fn island(p: [f32; 2]) -> bool { storm_zones::volumes().iter().any(|v| storm_zones::volume_encompasses(v, p)) }

pub fn pick_location(rng: &mut CrtRand, center: [f32; 3], radius: f32, inside: &dyn Fn([f32; 2]) -> bool) -> [f32; 3] {
    for _ in 0..PICK_TRIES {
        let o = storm_zones::rand_point_in_circle(rng, radius);
        let p = [o[0] + center[0], o[1] + center[1]];
        if inside(p) { return [p[0], p[1], center[2]]; }
    }
    center
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spawn { pub at: [f32; 3], pub yaw: f32, pub phase: usize }

#[derive(Clone, Debug)]
pub struct Schedule { pub next: Option<f32>, pub rng: CrtRand }

impl Schedule {
    pub fn new(seed: u32) -> Schedule { Schedule { next: None, rng: CrtRand(seed) } }
    pub fn arm(&mut self, now: f32, d: &Data) { self.next = Some(now + d.next_delay(&mut self.rng)); }
    pub fn tick(&mut self, now: f32, d: &Data, phase: usize, locations: &[[f32; 3]], inside: &dyn Fn([f32; 2]) -> bool) -> Vec<Spawn> {
        let Some(t) = self.next else { return vec![] };
        if now < t { return vec![]; }
        let mut out = vec![];
        if phase < locations.len() && phase < PHASE_COUNT {
            let n = d.count(phase, &mut self.rng);
            for _ in 0..n.max(0) {
                let at = pick_location(&mut self.rng, locations[phase], SAFE_ZONE_DEFS[phase].radius, inside);
                let yaw = self.rng.rand() as f32 * 0.010_986_663;
                out.push(Spawn { at, yaw, phase });
            }
        }
        self.arm(now, d);
        out
    }
}

pub fn lerp_fall_rate(t: f32) -> f32 {
    let (a, b) = (LERP_FALL_RATE[0], LERP_FALL_RATE[1]);
    let t = t.clamp(0., 1.);
    let h = b.0 - a.0;
    let (t2, t3) = (t * t, t * t * t);
    let (h00, h10, h01, h11) = (2. * t3 - 3. * t2 + 1., t3 - 2. * t2 + t, -2. * t3 + 3. * t2, t3 - t2);
    h00 * a.1 + h10 * h * a.3 + h01 * b.1 + h11 * h * b.2
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropEvent { DisableFlare, AlmostLanded, Finished, Landed }

#[derive(Clone, Debug)]
pub struct Fall {
    pub t: f32,
    pub rate: f32,
    pub base_rate: f32,
    pub popped_at: Option<f32>,
    pub finished_at: Option<f32>,
    pub landed: bool,
    pub flare_off: bool,
    pub almost: bool,
}

impl Fall {
    pub fn new(fall_time: f32) -> Fall { let r = 1. / fall_time.max(1e-3); Fall { t: 0., rate: r, base_rate: r, popped_at: None, finished_at: None, landed: false, flare_off: false, almost: false } }
    pub fn fall_curve(&self) -> f32 { 1. - self.t.clamp(0., 1.) }
    pub fn height(&self, fall_height: f32) -> f32 { fall_height * self.fall_curve() }
    pub fn pop(&mut self, now: f32) -> bool { if self.popped_at.is_some() || self.finished_at.is_some() { return false; } self.popped_at = Some(now); true }
    pub fn popped(&self) -> bool { self.popped_at.is_some() }
    pub fn step(&mut self, now: f32, dt: f32) -> Vec<DropEvent> {
        let mut ev = vec![];
        if let Some(p) = self.popped_at { let l = lerp_fall_rate(now - p); self.rate = self.base_rate + (POPPED_PLAY_RATE - self.base_rate) * l; }
        if self.finished_at.is_none() {
            self.t = (self.t + self.rate * dt).min(1.);
            if !self.flare_off && self.t >= DISABLE_FLARE_T { self.flare_off = true; ev.push(DropEvent::DisableFlare); }
            if !self.almost && self.t >= ALMOST_LANDED_T { self.almost = true; ev.push(DropEvent::AlmostLanded); }
            if self.t >= 1. { self.finished_at = Some(now); ev.push(DropEvent::Finished); }
        }
        if let Some(f) = self.finished_at { if !self.landed && now - f >= LAND_FX_DELAY { self.landed = true; ev.push(DropEvent::Landed); } }
        ev
    }
}

pub fn loot_offset(rng: &mut crate::containers::Rng) -> [f32; 3] {
    let r = LOOT_RADIUS_CM.0 + rng.next_f32() * (LOOT_RADIUS_CM.1 - LOOT_RADIUS_CM.0);
    let z = rng.next_f32() * 2. - 1.;
    let a = rng.next_f32() * std::f32::consts::TAU;
    let s = (1. - z * z).max(0.).sqrt();
    [s * a.cos() * r + LOOT_SPAWN_OFFSET_CM[0], s * a.sin() * r + LOOT_SPAWN_OFFSET_CM[1], z * r + LOOT_SPAWN_OFFSET_CM[2]]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paks() -> Option<PakSet> { PakSet::open_install(r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame").ok() }
    #[test] fn the_cooked_schedule_is_three_minutes_give_or_take_thirty_seconds() {
        let Some(set) = paks() else { return };
        let d = Data::load(&set).unwrap();
        assert_eq!(d, Data::default());
        let mut rng = CrtRand(9);
        for _ in 0..500 { let x = d.next_delay(&mut rng); assert!((150. ..=210.).contains(&x), "{x}"); }
    }
    #[test] fn the_counts_follow_the_phase_rows() {
        let d = Data::default();
        let mut rng = CrtRand(3);
        for _ in 0..200 { assert_eq!(d.count(0, &mut rng), 2); assert!((0..=2).contains(&d.count(1, &mut rng))); assert!((0..=1).contains(&d.count(5, &mut rng))); assert_eq!(d.count(8, &mut rng), 0); }
        let mut seen = [false; 3];
        for _ in 0..500 { seen[d.count(2, &mut rng) as usize] = true; }
        assert_eq!(seen, [true; 3]);
    }
    #[test] fn a_drop_lands_inside_the_zone_and_on_the_island() {
        let d = Data::default();
        let mut s = Schedule::new(11);
        s.arm(0., &d);
        let locs = vec![storm_zones::MAP_INFO_ROOT_CM; PHASE_COUNT];
        assert!(s.tick(100., &d, 0, &locs, &island).is_empty());
        let out = s.tick(300., &d, 0, &locs, &island);
        assert_eq!(out.len(), 2);
        for sp in &out {
            let (dx, dy) = (sp.at[0] - locs[0][0], sp.at[1] - locs[0][1]);
            assert!((dx * dx + dy * dy).sqrt() <= SAFE_ZONE_DEFS[0].radius + 1.);
            assert!(island([sp.at[0], sp.at[1]]));
            assert!((0. ..360.01).contains(&sp.yaw));
        }
        assert!(s.next.unwrap() >= 300. + 150.);
    }
    #[test] fn the_crate_falls_for_the_fall_time_and_a_popped_balloon_ends_it_fast() {
        let mut f = Fall::new(60.);
        let mut ev = vec![];
        let mut now = 0.;
        while now < 70. && !f.landed { now += 0.05; ev.extend(f.step(now, 0.05)); }
        assert!((f.finished_at.unwrap() - 60.).abs() < 0.11);
        assert_eq!(ev, vec![DropEvent::DisableFlare, DropEvent::AlmostLanded, DropEvent::Finished, DropEvent::Landed]);
        let mut g = Fall::new(60.);
        let mut now = 0.;
        while now < 10. { now += 0.05; g.step(now, 0.05); }
        let h = g.height(10_000.);
        assert!(g.pop(now));
        let t0 = now;
        while g.finished_at.is_none() { now += 0.05; g.step(now, 0.05); }
        assert!(now - t0 < 1.5, "{} s from {h} cm", now - t0);
        assert!((lerp_fall_rate(0.) - 0.).abs() < 1e-6 && (lerp_fall_rate(1.) - 1.).abs() < 1e-6);
    }
}
