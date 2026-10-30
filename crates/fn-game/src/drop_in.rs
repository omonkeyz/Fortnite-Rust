use crate::sim::{GameSim, TickInput, EYE, Z_OFFSET};
use fn_camera::{self as camera, Mode, Sample, View};
use fn_movement::skydive::Aerial;
use glam::{Vec2, Vec3};

pub const MAP_INFO_LOCATION: [f32; 3] = [32224., -25744., 1312.];
pub const AIRCRAFT_SPAWN_ZONE_HALF: f32 = 160000.;
pub const AIRCRAFT_HEIGHT: f32 = 80000.;
pub const AIRCRAFT_SPEED: f32 = 6000.;
pub const AIRCRAFT_DEVIATION_ANGLE: f32 = 45.;
pub const ISLAND_VOLUME_LOCATION: [f32; 2] = [32512., -24192.];
pub const ISLAND_HULLS: [&[[f32; 2]]; 8] = [
    &[[89856., 81536.], [75264., 95872.], [-3072.1, 112256.], [-26368., 105600.], [-104639.6, 10770.9], [-103680., -8832.], [87214.2, -66782.]],
    &[[105472., -48768.], [88064., -19072.], [87214.2, -66782.], [104348.7, -71983.5]],
    &[[-61696., 96896.], [-86528., 88704.], [-99072., 71040.], [-88020.5, 30905.6], [-46042.2, 81764.]],
    &[[-54016., 89472.], [-46042.2, 81764.], [-39680., 89472.]],
    &[[-105472., 27776.], [-104639.6, 10771.], [-88020.5, 30905.6], [-89344., 35712.]],
    &[[-13568., -112256.], [14592., -111744.], [89993.9, -67625.8], [-61966.9, -21494.9], [-49664., -69760.]],
    &[[93952., -91008.], [103936., -80511.9], [104348.7, -71983.6], [89993.9, -67625.8], [38655.9, -97664.]],
    &[[-97280., -33664.], [-92672.1, -45696.], [-56320., -43648.], [-61966.9, -21494.9], [-89344., -13184.]],
];
pub const CAMERA_INITIAL_PITCH: f32 = -30.;
pub const WARMUP_COUNTDOWN_DURATION: f32 = 90.;
pub const GET_READY_SECONDS: i32 = 10;
pub const SPAWN_ISLAND_LOCATION: [f32; 3] = [-125440., -113664., 3840.];
pub const PAWN_HALF_HEIGHT: f32 = 75.;
pub const WARMUP_STARTS: [[f32; 4]; 110] = [
    [8825.3, -7897.2, 92., 90.], [8537.3, -7897.2, 92., 90.], [8585.3, 10230.8, 92., -90.], [8873.3, 10230.8, 92., -90.], [775., 9680.8, 123.7, -67.5], [1041.1, 9791., 122.5, -67.5],
    [8973.3, -8225.2, 92., 90.], [8685.3, -8225.2, 92., 90.], [8437.3, 10558.8, 92., -90.], [8397.3, -8225.2, 92., 90.], [8725.3, 10558.8, 92., -90.], [8973.3, -7577.2, 92., 90.],
    [8685.3, -7577.2, 92., 90.], [9013.3, 10558.8, 92., -90.], [8397.3, -7577.2, 92., 90.], [8437.3, 9910.8, 92., -90.], [8725.3, 9910.8, 92., -90.], [9013.3, 9910.8, 92., -90.],
    [512.8, 9927.2, 122.5, -67.5], [778.9, 10037.3, 122.5, -67.5], [1044.9, 10147.6, 122.5, -67.5], [760.8, 9328.5, 124.3, -67.5], [1026.9, 9438.7, 124.2, -67.5], [1292.9, 9548.9, 122.5, -67.5],
    [3259.9, -11862.6, -180.9, 135.], [3056.3, -12066.2, -195.4, 135.], [3596.5, -11989.9, -140.3, 135.], [3392.8, -12193.5, -168.9, 135.], [3189.2, -12397.2, -193.9, 135.], [3138.3, -11531.6, -216.4, 135.],
    [2934.6, -11735.3, -212.1, 135.], [2731., -11938.9, -212.3, 135.], [-8502.7, -14441.2, 1468., 90.], [-8790.7, -14441.2, 1468., 90.], [-8354.7, -14769.2, 1494.6, 90.], [-8642.7, -14769.2, 1472.5, 90.],
    [-8930.7, -14769.2, 1472.1, 90.], [-8354.7, -14121.2, 1466.4, 90.], [-8642.7, -14121.2, 1466.1, 90.], [-8930.7, -14121.2, 1466.2, 90.], [-14754.7, -7517.2, -50.9, 0.], [-14754.7, -7229.2, -48.9, 0.],
    [-15082.7, -7665.2, -45.6, 0.], [-15082.7, -7377.2, -46.2, 0.], [-15082.7, -7089.2, -45.5, 0.], [-14434.7, -7665.2, -82., 0.], [-14434.7, -7377.2, -70., 0.], [-14434.7, -7089.2, -66.7, 0.],
    [-18434.7, 1474.8, 484.2, 0.], [-18434.7, 1762.8, 486., 0.], [-18762.7, 1326.8, 484.2, 0.], [-18762.7, 1614.8, 486., 0.], [-18762.7, 1902.8, 484.2, 0.], [-18114.7, 1326.8, 484.2, 0.],
    [-18114.7, 1614.8, 486., 0.], [-18114.7, 1902.8, 486., 0.], [-7838., -736.2, -81.9, 45.], [-8041.6, -532.5, -88.8, 45.], [-7965.3, -1072.7, -102.1, 45.], [-8169., -869.1, -110.7, 45.],
    [-8372.6, -665.5, -113.7, 45.], [-7507., -614.5, -55., 45.], [-7710.8, -410.9, -63.5, 45.], [-7914.4, -207.2, -74.6, 45.], [-3028., 2746.2, 109.3, -45.], [-2824.4, 2949.8, 105.4, -45.],
    [-3364.6, 2873.5, 111.4, -45.], [-3161., 3077.2, 111.1, -45.], [-2957.3, 3280.8, 108.6, -45.], [-2906.4, 2415.3, 107.6, -45.], [-2702.8, 2619., 105.4, -45.], [-2499.1, 2822.5, 107.1, -45.],
    [1106., -2272.2, 109.3, 45.], [902.4, -2068.5, 105.4, 45.], [978.7, -2608.7, 111.4, 45.], [775., -2405.1, 111.1, 45.], [571.4, -2201.5, 108.6, 45.], [1437., -2150.6, 107.6, 45.],
    [1233.2, -1946.9, 105.4, 45.], [1029.7, -1743.2, 107.1, 45.], [9129.3, -7897.2, 92., 90.], [8233.3, -7897.2, 92., 90.], [8281.3, 10230.8, 92., -90.], [9177.3, 10230.8, 92., -90.],
    [494.2, 9564.5, 122.5, -67.5], [1322., 9907.3, 122.5, -67.5], [3474.9, -11647.6, -167.2, 135.], [2841.3, -12281.2, -208.2, 135.], [-8198.7, -14441.2, 1476.5, 90.], [-9094.7, -14441.2, 1468., 90.],
    [-14754.7, -7821.2, -58., 0.], [-14754.7, -6925.2, -48.7, 0.], [-18434.7, 1170.8, 484.2, 0.], [-18434.7, 2066.8, 484.2, 0.], [-7623.1, -951.1, -70.4, 45.], [-8256.6, -317.5, -98.3, 45.],
    [-3243., 2531.3, 111.4, -45.], [-2609.4, 3164.8, 105.7, -45.], [1320.9, -2487.1, 111.4, 45.], [687.4, -1853.5, 105.7, 45.], [-14175.6, -7376.2, -99.7, 0.], [-8640., -13824., 1436., 90.],
    [2739.2, -11530.4, -223.3, 135.], [8688., -7264., 106.5, 90.], [8720., 9616., 92., -90.], [1152., 9152., 92., -67.5], [-2512., 2416., 108., -45.], [-7462.4, -177.3, -47.9, 45.],
    [-17825.4, 1616., 470.9, 0.], [1472., -1728., 92., 45.],
];

pub const SAFE_ZONE_START_DELAY: f32 = 60.;
pub const SKYDIVING_SCAN_FOR_IMPACT_WORLD_Z: f32 = 20000.;
pub const SKYDIVING_FORCE_DEPLOY_PARACHUTE_WORLD_Z: f32 = 0.;
pub const PARACHUTE_DEPLOY_TRACE_FOR_GROUND_DISTANCE: f32 = 10000.;
pub const PARACHUTE_DEPLOY_VELOCITY_BLEND: f32 = 0.3;
pub const PAWN_CAPSULE_RADIUS: f32 = 42.;
pub const PARACHUTE_DEPLOY_FIXED_VERTICAL_DISTANCE: bool = true;

pub const AIRCRAFT_CAMERA_FOV: f32 = 90.;
pub const AIRCRAFT_VIEW_TARGET_OFFSET: [f32; 3] = [-45., 0., 300.];
pub const AIRCRAFT_CAMERA_SPACE_OFFSET: [Sample; 3] = [Sample { pitch: -90., offset: [-2200., 0., 0.] }, Sample { pitch: 0., offset: [-1800., 0., 0.] }, Sample { pitch: 90., offset: [-1200., 0., 0.] }];

pub const SKYDIVE_GLIDE: Mode = Mode {
    name: "Athena_PlayerCameraModeSkydiveGlide", fov: 90., forward_distance: -280.,
    origin_interp: [30., 1.0e11, 60.],
    vt_offset: &[Sample { pitch: 0., offset: [0., 0., 75.] }],
    vt_offset_crouch: &[Sample { pitch: 0., offset: [0., 0., 75.] }],
    vt_interp: [8., 8., 8.],
    cs_offset: &[Sample { pitch: 0., offset: [-30., 0., 0.] }, Sample { pitch: 90., offset: [-75., 0., 25.] }, Sample { pitch: -90., offset: [100., 0., 50.] }],
    push_out: 0.2, hide_pawn_pct: 0., transition_time: 1.0, feelers: &camera::FEELERS,
};
pub const SKYDIVE_DIVE: Mode = Mode {
    name: "Athena_PlayerCameraModeSkydiveDive", fov: 100., forward_distance: -280.,
    origin_interp: [30., 1.0e11, 60.],
    vt_offset: &[Sample { pitch: 0., offset: [0., 0., 75.] }],
    vt_offset_crouch: &[Sample { pitch: 0., offset: [0., 0., 75.] }],
    vt_interp: [8., 8., 8.],
    cs_offset: &[Sample { pitch: 0., offset: [0., 0., 0.] }, Sample { pitch: 90., offset: [0., 0., 0.] }, Sample { pitch: -90., offset: [150., 0., 25.] }],
    push_out: 0.2, hide_pawn_pct: 0., transition_time: 2.0, feelers: &camera::FEELERS,
};
pub const SKYDIVE_PARACHUTE: Mode = Mode {
    name: "Athena_PlayerCameraModeSkydiveParachute", fov: 80., forward_distance: -280.,
    origin_interp: [30., 1.0e11, 60.],
    vt_offset: &[Sample { pitch: 0., offset: [-75., 0., 145.] }],
    vt_offset_crouch: &[Sample { pitch: 0., offset: [-75., 0., 145.] }],
    vt_interp: [8., 8., 8.],
    cs_offset: &[Sample { pitch: 0., offset: [0., 0., 0.] }, Sample { pitch: 90., offset: [-75., 0., 0.] }, Sample { pitch: -90., offset: [-145., 0., -25.] }],
    push_out: 0.2, hide_pawn_pct: 0., transition_time: 1.0, feelers: &camera::FEELERS,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamePhase { Warmup, Aircraft, SafeZones }
pub fn damage_enabled(phase: Option<GamePhase>) -> bool { phase != Some(GamePhase::Warmup) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamePhaseStep { None, Warmup, GetReady, BusLocked, BusFlying, StormForming }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerialPhase { None, BusCantExit, BusCanExit, Skydiving, Parachuting }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropEvent { EnteredAircraft, DoorsOpened, ExitedAircraft { auto: bool }, AircraftExitedDropZone, FlightEnded, ParachuteOpened, ParachuteClosed, Landed }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlightPath { pub start: Vec3, pub yaw_deg: f32, pub speed: f32, pub duration: f32, pub drop_start_offset: f32, pub drop_end_offset: f32, pub enter: Vec2, pub exit: Vec2 }

fn sin_cos_deg(a: f32) -> (f32, f32) { a.to_radians().sin_cos() }

fn convex_hull(points: &[[f32; 2]]) -> Vec<Vec2> {
    let mut p: Vec<Vec2> = points.iter().map(|q| Vec2::new(q[0], q[1])).collect();
    p.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap().then(a.y.partial_cmp(&b.y).unwrap()));
    p.dedup_by(|a, b| (*a - *b).length() < 1e-3);
    if p.len() < 3 { return p; }
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    let mut lower: Vec<Vec2> = vec![];
    for &q in &p { while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], q) <= 0. { lower.pop(); } lower.push(q); }
    let mut upper: Vec<Vec2> = vec![];
    for &q in p.iter().rev() { while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], q) <= 0. { upper.pop(); } upper.push(q); }
    lower.pop(); upper.pop(); lower.extend(upper);
    lower
}

fn segment_enters_convex(a: Vec2, b: Vec2, poly: &[Vec2]) -> Option<f32> {
    let d = b - a;
    let (mut t0, mut t1) = (0f32, 1f32);
    let n = poly.len();
    for i in 0..n {
        let (p, q) = (poly[i], poly[(i + 1) % n]);
        let e = q - p;
        let normal = Vec2::new(e.y, -e.x);
        let num = normal.dot(p - a);
        let den = normal.dot(d);
        if den.abs() < 1e-9 { if num < 0. { return None; } continue; }
        let t = num / den;
        if den < 0. { t0 = t0.max(t); } else { t1 = t1.min(t); }
        if t0 > t1 { return None; }
    }
    Some(t0)
}

pub fn trace_island_volume(from: Vec2, to: Vec2) -> Option<Vec2> {
    let off = Vec2::from(ISLAND_VOLUME_LOCATION);
    let mut best: Option<f32> = None;
    for h in ISLAND_HULLS.iter() {
        let poly: Vec<Vec2> = convex_hull(h).into_iter().map(|p| p + off).collect();
        if let Some(t) = segment_enters_convex(from, to, &poly) { best = Some(best.map_or(t, |b: f32| b.min(t))); }
    }
    best.map(|t| from + (to - from) * t)
}

fn island_bounds() -> (Vec2, Vec2) {
    let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
    for h in ISLAND_HULLS.iter() { for p in h.iter() { let v = Vec2::from(*p); lo = lo.min(v); hi = hi.max(v); } }
    let off = Vec2::from(ISLAND_VOLUME_LOCATION);
    let half = (hi - lo) * 0.5;
    let ext = Vec2::new(half.x.max(lo.x.abs()).max(hi.x.abs()), half.y.max(lo.y.abs()).max(hi.y.abs()));
    (off, ext)
}

pub fn initialize_flight_path(u_angle: f32, u_deviation: f32) -> FlightPath {
    let m = Vec3::from(MAP_INFO_LOCATION);
    let (s_lo, s_hi) = (Vec2::new(m.x, m.y) - AIRCRAFT_SPAWN_ZONE_HALF, Vec2::new(m.x, m.y) + AIRCRAFT_SPAWN_ZONE_HALF);
    let z = AIRCRAFT_HEIGHT + m.z;
    let (c, e) = island_bounds();
    let r = e.x.max(e.y);
    let a1 = u_angle.clamp(0., 1.) * 360.;
    let (s1, c1) = sin_cos_deg(a1);
    let p1 = Vec2::new(c.x + r * s1, c.y + r * c1);
    let a2 = a1 + 180. + (-AIRCRAFT_DEVIATION_ANGLE + (2. * AIRCRAFT_DEVIATION_ANGLE) * u_deviation.clamp(0., 1.));
    let (s2, c2) = sin_cos_deg(a2);
    let p2 = Vec2::new(c.x + r * s2, c.y + r * c2);
    let d = p2 - p1;
    let (mut tmin, mut tmax) = (f32::MIN, f32::MAX);
    for k in 0..2 {
        if d[k].abs() <= 1e-8 { continue; }
        let (ta, tb) = ((s_lo[k] - p1[k]) / d[k], (s_hi[k] - p1[k]) / d[k]);
        tmin = tmin.max(ta.min(tb)); tmax = tmax.min(ta.max(tb));
    }
    let start = p1 + d * tmin;
    let end = p1 + d * tmax;
    let dir = d.normalize_or_zero();
    let l = 2. * (e.x * e.x + e.y * e.y).sqrt() + 1000.;
    let enter = trace_island_volume(p2 - dir * l, p2).unwrap_or(p1);
    let exit = trace_island_volume(p1 + dir * l, p1).unwrap_or(p2);
    let u = (end - start).normalize_or_zero();
    let len = (end - start).length();
    let v = AIRCRAFT_SPEED;
    let drop_start_offset = ((enter - start).dot(u) / v).max(0.);
    let drop_end_offset = drop_start_offset.max((exit - start).dot(u) / v);
    FlightPath { start: Vec3::new(start.x, start.y, z), yaw_deg: u.y.atan2(u.x).to_degrees(), speed: v, duration: len / v, drop_start_offset, drop_end_offset, enter, exit }
}

impl FlightPath {
    pub fn forward(&self) -> Vec3 { let (s, c) = sin_cos_deg(self.yaw_deg); Vec3::new(c, s, 0.) }
    pub fn location(&self, t: f32) -> Vec3 { self.start + self.forward() * self.speed * t }
}

#[derive(Clone, Debug)]
pub struct Aircraft { pub path: FlightPath, pub start_time: f32, pub in_drop_zone: bool, pub flight_ended: bool, pub locked: bool }
impl Aircraft {
    pub fn drop_start_time(&self) -> f32 { self.start_time + self.path.drop_start_offset }
    pub fn drop_end_time(&self) -> f32 { self.start_time + self.path.drop_end_offset }
    pub fn flight_end_time(&self) -> f32 { self.start_time + self.path.duration }
    pub fn location_ue(&self, now: f32) -> Vec3 { self.path.location(now - self.start_time) }
}

#[derive(Clone, Debug)]
pub struct DropIn {
    pub map: fn_terrain::Mapping,
    pub aircraft: Option<Aircraft>,
    pub phase: GamePhase,
    pub safe_zones_start_time: f32,
    pub now: f32,
    pub view_time: f32,
    pub in_aircraft: bool,
    pub has_exited: bool,
    pub below_test_height: bool,
    pub events: Vec<DropEvent>,
    pub cam_mode: Option<&'static Mode>,
    pub cam_from: Option<&'static Mode>,
    pub cam_blend: f32,
    pub prev_mode: Aerial,
    pub pending_path: Option<FlightPath>,
    pub warmup_end: f32,
}

pub fn identity_mapping() -> fn_terrain::Mapping { fn_terrain::Mapping { centre_cm: [0.; 3], anchor: Vec3::ZERO, ref_z_cm: 0. } }

impl DropIn {
    pub fn new(map: fn_terrain::Mapping, path: FlightPath) -> DropIn {
        DropIn { map, aircraft: Some(Aircraft { path, start_time: 0., in_drop_zone: false, flight_ended: false, locked: true }), phase: GamePhase::Aircraft, safe_zones_start_time: -1., now: 0., view_time: 0.,
            in_aircraft: true, has_exited: false, below_test_height: false, events: vec![DropEvent::EnteredAircraft], cam_mode: None, cam_from: None, cam_blend: 1., prev_mode: Aerial::None, pending_path: None, warmup_end: -1. }
    }
    pub fn new_warmup(map: fn_terrain::Mapping, path: FlightPath, countdown: f32) -> DropIn {
        let mut d = DropIn::new(map, path);
        d.aircraft = None; d.pending_path = Some(path); d.phase = GamePhase::Warmup; d.in_aircraft = false; d.events.clear(); d.warmup_end = countdown;
        d
    }
    pub fn tick_warmup(&mut self, h: f32) -> bool {
        self.now += h;
        if self.view_time < self.now - 0.25 || self.view_time > self.now + 0.25 { self.view_time = self.now; }
        if self.now <= self.warmup_end { return false; }
        let Some(path) = self.pending_path.take() else { return false };
        self.aircraft = Some(Aircraft { path, start_time: self.now, in_drop_zone: false, flight_ended: false, locked: true });
        self.phase = GamePhase::Aircraft; self.in_aircraft = true; self.view_time = self.now;
        self.events.push(DropEvent::EnteredAircraft);
        true
    }
    pub fn take_events(&mut self) -> Vec<DropEvent> { std::mem::take(&mut self.events) }
    pub fn ue_to_game(&self, ue: Vec3) -> Vec3 { self.map.to_game(ue) }
    pub fn game_yaw(&self, ue_yaw: f32) -> f32 { (ue_yaw + 90.).rem_euclid(360.) }
    pub fn aircraft_game(&self, t: f32) -> Option<(Vec3, f32)> { self.aircraft.as_ref().map(|a| (self.ue_to_game(a.location_ue(t)), self.game_yaw(a.path.yaw_deg))) }
    pub fn aircraft_view(&self) -> Option<(Vec3, f32)> { self.aircraft_game(self.view_time) }
    pub fn game_phase_step(&self) -> (GamePhaseStep, f32) {
        let now = self.now;
        match self.phase {
            GamePhase::Warmup if now < self.warmup_end => { let r = (self.warmup_end - now).floor(); (if r as i32 <= GET_READY_SECONDS { GamePhaseStep::GetReady } else { GamePhaseStep::Warmup }, r) }
            GamePhase::Warmup => (GamePhaseStep::None, 0.),
            GamePhase::Aircraft => match &self.aircraft {
                Some(a) if now < a.drop_start_time() => (GamePhaseStep::BusLocked, a.drop_start_time() - now + 1.),
                Some(a) if now < a.drop_end_time() => (GamePhaseStep::BusFlying, a.drop_end_time() - now),
                _ => (GamePhaseStep::None, 0.),
            },
            GamePhase::SafeZones if now < self.safe_zones_start_time => (GamePhaseStep::StormForming, self.safe_zones_start_time - now),
            GamePhase::SafeZones => (GamePhaseStep::None, 0.),
        }
    }
    pub fn tick_aircraft(&mut self, h: f32) -> bool {
        self.now += h;
        if self.view_time < self.now - 0.25 || self.view_time > self.now + 0.25 { self.view_time = self.now; }
        let now = self.now;
        let mut kick = false;
        let Some(a) = self.aircraft.as_mut() else { return false };
        if a.flight_ended { return false; }
        let doors = a.drop_start_time() < now && now < a.drop_end_time();
        if doors && !a.in_drop_zone { a.in_drop_zone = true; a.locked = false; self.events.push(DropEvent::DoorsOpened); }
        if !doors && a.in_drop_zone { a.in_drop_zone = false; kick = true; }
        if now > a.flight_end_time() {
            a.flight_ended = true;
            if a.in_drop_zone { a.in_drop_zone = false; kick = true; }
        }
        let ended = a.flight_ended;
        if kick {
            self.events.push(DropEvent::AircraftExitedDropZone);
            if self.phase != GamePhase::SafeZones { self.phase = GamePhase::SafeZones; self.safe_zones_start_time = now + SAFE_ZONE_START_DELAY; }
        }
        if ended { self.events.push(DropEvent::FlightEnded); self.aircraft = None; }
        kick && self.in_aircraft
    }
    pub fn can_jump(&self) -> bool { self.in_aircraft && self.aircraft.as_ref().map_or(false, |a| !a.locked) }
}

pub fn aircraft_view(bus: Vec3, bus_yaw: f32, yaw: f32, pitch: f32) -> View {
    let pitch = camera::clamp_pitch(pitch);
    let (bf, _br) = camera::yaw_basis(bus_yaw);
    let vt = bf * AIRCRAFT_VIEW_TARGET_OFFSET[0] * camera::CM + Vec3::Y * AIRCRAFT_VIEW_TARGET_OFFSET[2] * camera::CM;
    let (forward, right, up) = camera::view_basis(yaw, pitch);
    let cs = camera::sample_offset(&AIRCRAFT_CAMERA_SPACE_OFFSET, pitch);
    let eye = bus + vt + (forward * cs.x + right * cs.y + up * cs.z) * camera::CM;
    View { eye, forward, right, up, yaw_deg: yaw, pitch_deg: pitch, safe: bus + vt, desired: eye, blocked_pct: 1., hide_pawn: true }
}

fn ease_in_out(t: f32) -> f32 { let t = t.clamp(0., 1.); if t < 0.5 { 2. * t * t } else { 1. - 2. * (1. - t) * (1. - t) } }

impl GameSim {
    pub fn start_match_on_bus(&mut self, u_angle: f32, u_deviation: f32) {
        let map = self.terrain().map(|w| w.map).unwrap_or_else(identity_mapping);
        let path = initialize_flight_path(u_angle, u_deviation);
        let mut d = DropIn::new(map, path);
        d.view_time = 0.;
        let (pos, yaw) = d.aircraft_game(0.).unwrap();
        self.yaw = yaw; self.pitch = CAMERA_INITIAL_PITCH;
        self.x = pos.x; self.y = pos.y; self.z = pos.z;
        self.cam_view = aircraft_view(pos, yaw, self.yaw, self.pitch);
        self.drop = Some(d);
    }
    pub fn start_match_on_spawn_island(&mut self, u_start: f32, u_angle: f32, u_deviation: f32, countdown: f32) {
        let map = self.terrain().map(|w| w.map).unwrap_or_else(identity_mapping);
        let d = DropIn::new_warmup(map, initialize_flight_path(u_angle, u_deviation), countdown);
        let i = ((u_start.clamp(0., 1.) * WARMUP_STARTS.len() as f32) as usize).min(WARMUP_STARTS.len() - 1);
        let s = WARMUP_STARTS[i];
        let o = Vec3::from(SPAWN_ISLAND_LOCATION);
        let feet = d.ue_to_game(Vec3::new(s[0] + o.x, s[1] + o.y, s[2] + o.z - PAWN_HALF_HEIGHT));
        self.drop = Some(d);
        self.yaw = (s[3] + 90.).rem_euclid(360.); self.pitch = 0.;
        self.controller = self.new_controller();
        self.controller.position = Vec2::new(feet.x, feet.z - Z_OFFSET);
        self.controller.height = feet.y;
        self.controller.facing = self.yaw;
        self.x = feet.x; self.y = feet.y + EYE; self.z = feet.z;
        self.sync_terrain();
        self.stand_on_floor();
        self.fall.reset();
        self.cam.reset();
    }
    pub fn in_warmup(&self) -> bool { self.drop.as_ref().map_or(false, |d| d.phase == GamePhase::Warmup) }
    pub fn match_phase(&self) -> Option<GamePhase> { self.drop.as_ref().map(|d| d.phase) }
    pub fn damage_enabled(&self) -> bool { damage_enabled(self.match_phase()) }
    pub fn on_bus(&self) -> bool { self.drop.as_ref().map_or(false, |d| d.in_aircraft) }
    pub fn aerial_phase(&self) -> AerialPhase {
        let Some(d) = &self.drop else { return AerialPhase::None };
        if d.in_aircraft {
            return match &d.aircraft { Some(a) if d.now < a.drop_start_time() => AerialPhase::BusCantExit, Some(a) if d.now < a.drop_end_time() => AerialPhase::BusCanExit, _ => AerialPhase::None };
        }
        let c = self.controller.component();
        if !c.is_skydiving() || c.is_parachute_forced_open() { return AerialPhase::None; }
        if c.is_parachute_open() { AerialPhase::Parachuting } else { AerialPhase::Skydiving }
    }
    pub fn items_blocked(&self) -> bool { self.dbno || self.on_bus() || self.controller.component().is_skydiving() || self.controller.component().in_aerial_mode() }
    pub fn drop_begin_frame(&mut self, dt: f32) {
        if let Some(d) = self.drop.as_mut() { d.view_time += dt; }
    }
    pub(crate) fn drop_tick_aircraft(&mut self, t: &TickInput, h: f32) -> bool {
        let jump_edge = t.jump && !self.bus_jump_prev;
        let Some(d) = self.drop.as_mut() else { return false };
        if d.phase == GamePhase::Warmup {
            if !d.tick_warmup(h) { return false; }
            let Some((pos, yaw)) = d.aircraft_game(d.now) else { return false };
            self.yaw = yaw; self.pitch = CAMERA_INITIAL_PITCH;
            self.x = pos.x; self.y = pos.y; self.z = pos.z;
            self.bus_jump_prev = t.jump;
            self.cam_view = aircraft_view(pos, yaw, self.yaw, self.pitch);
            return true;
        }
        if !d.in_aircraft { d.tick_aircraft(h); return false; }
        self.bus_jump_prev = t.jump;
        let at = d.now + h;
        let state = d.aircraft.as_ref().map(|a| (a.location_ue(at), a.path.forward() * a.path.speed));
        let kicked = d.tick_aircraft(h);
        let jump = kicked || (jump_edge && d.can_jump());
        match state {
            Some((loc, vel)) if jump => { self.exit_aircraft(loc, vel, kicked); false }
            Some((loc, _)) => { let p = d.ue_to_game(loc); self.x = p.x; self.y = p.y; self.z = p.z; true }
            None => { d.in_aircraft = false; d.has_exited = true; false }
        }
    }
    fn exit_aircraft(&mut self, loc_ue: Vec3, vel_ue: Vec3, auto: bool) {
        let d = self.drop.as_mut().unwrap();
        d.in_aircraft = false; d.has_exited = true; d.below_test_height = false;
        d.events.push(DropEvent::ExitedAircraft { auto });
        let p = d.ue_to_game(loc_ue);
        let mut c = self.new_controller();
        c.position = Vec2::new(p.x, p.z - Z_OFFSET);
        c.height = p.y;
        c.velocity = Vec2::new(vel_ue.x, vel_ue.y) * 0.01;
        c.vertical_speed = 0.;
        c.facing = self.yaw; c.actor_yaw = Some(self.yaw);
        self.controller = c;
        self.controller.component_mut().start_skydiving();
        self.sync_terrain();
        self.fall.reset();
        self.fall.ignore_next = true;
        self.cam.reset();
        self.x = p.x; self.y = p.y + EYE; self.z = p.z;
    }
    pub(crate) fn drop_before_step(&mut self, t: &TickInput) {
        let Some(d) = self.drop.as_mut() else { return };
        if d.in_aircraft { return; }
        let jump_edge = t.jump && !self.bus_jump_prev;
        self.bus_jump_prev = t.jump;
        let (yaw, pitch) = (self.yaw.to_radians(), self.pitch.to_radians());
        let fwd_game = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos());
        let right_game = Vec3::new(yaw.cos(), 0., yaw.sin());
        let g2c = |v: Vec3| Vec3::new(v.x, v.z, v.y);
        let mut input = fwd_game * t.forward + right_game * t.side;
        if input.length_squared() > 1. { input = input.normalize(); }
        let cmc = self.controller.component_mut();
        cmc.set_aerial_input(g2c(input), g2c(fwd_game));
        if jump_edge { cmc.on_jump_pressed_aerial(); }
        self.drop_auto_deploy();
    }
    fn drop_auto_deploy(&mut self) {
        let Some(d) = self.drop.as_ref() else { return };
        let c = self.controller.component();
        if !c.is_skydiving() || c.is_parachute_forced_open() { return; }
        let map = d.map;
        let p = self.controller.render_position();
        let feet_game = Vec3::new(p.x, self.controller.height, p.y + Z_OFFSET);
        let actor_ue = map.to_ue(feet_game) + Vec3::new(0., 0., c.half_height);
        if !d.below_test_height {
            if actor_ue.z <= SKYDIVING_SCAN_FOR_IMPACT_WORLD_Z { self.drop.as_mut().unwrap().below_test_height = true; } else { return; }
        }
        let vel = c.velocity;
        let b = PARACHUTE_DEPLOY_VELOCITY_BLEND.clamp(0., 1.);
        let vn = vel.normalize_or_zero();
        let dir = (Vec3::new(0., 0., -1.).lerp(vn, b)).normalize_or_zero();
        let mut dist = PARACHUTE_DEPLOY_TRACE_FOR_GROUND_DISTANCE;
        if PARACHUTE_DEPLOY_FIXED_VERTICAL_DISTANCE && b < 0.99 && dir.z < -1e-4 { dist /= dir.z.abs(); }
        let to_ue = actor_ue + dir * dist;
        let a = map.to_game(actor_ue);
        let bpt = map.to_game(to_ue);
        let r = 2. * PAWN_CAPSULE_RADIUS * 0.01;
        let hit = self.world_sweep(a, bpt, r).is_some();
        let low = actor_ue.z <= SKYDIVING_FORCE_DEPLOY_PARACHUTE_WORLD_Z + PARACHUTE_DEPLOY_TRACE_FOR_GROUND_DISTANCE;
        if hit || low { self.controller.component_mut().force_parachute_open(); }
    }
    pub fn world_sweep(&self, a: Vec3, b: Vec3, r: f32) -> Option<f32> {
        let mid = (a + b) * 0.5;
        let reach = (b - a).length() * 0.5 + r + 1.;
        let mut ids = vec![];
        match self.fresh_prop_index() {
            Some(ix) => { ix.hull_candidates(Vec2::new(mid.x, mid.z), reach, &mut ids); ids.extend_from_slice(ix.plain()); }
            None => ids = (0..self.world.props.len() as u32).collect(),
        }
        let hulls = Self::camera_hulls(&self.building_hulls, &self.door_hulls);
        let world = Self::camera_world(&self.world.props, &ids, &hulls, self.terrain());
        world(a, b, r)
    }
    pub(crate) fn drop_after_step(&mut self) {
        if self.in_warmup() { let _ = self.fall.take_landings(); }
        let Some(d) = self.drop.as_mut() else { return };
        if d.in_aircraft { return; }
        let c = self.controller.component();
        let mode = c.aerial_mode();
        if mode != d.prev_mode {
            match (d.prev_mode, mode) {
                (_, Aerial::Parachute) => d.events.push(DropEvent::ParachuteOpened),
                (Aerial::Parachute, Aerial::Skydive) => d.events.push(DropEvent::ParachuteClosed),
                (Aerial::Parachute, Aerial::None) | (Aerial::Skydive, Aerial::None) => d.events.push(DropEvent::Landed),
                _ => {}
            }
            d.prev_mode = mode;
        }
        if mode == Aerial::Parachute { self.fall.apex_m = self.controller.height; }
    }
    pub fn drop_camera_mode(&self) -> Option<&'static Mode> {
        let d = self.drop.as_ref()?;
        if d.in_aircraft { return None; }
        let c = self.controller.component();
        if c.is_parachute_open() { return Some(&SKYDIVE_PARACHUTE); }
        if c.is_actively_skydiving() { return Some(&SKYDIVE_DIVE); }
        if c.is_skydiving() { return Some(&SKYDIVE_GLIDE); }
        None
    }
    pub(crate) fn drop_update_camera_blend(&mut self, h: f32) {
        let target = self.drop_camera_mode();
        let base = self.ads.tick_mode();
        let Some(d) = self.drop.as_mut() else { return };
        let cur = target.unwrap_or(base);
        let prev = d.cam_mode.unwrap_or(base);
        if prev.name != cur.name {
            d.cam_from = Some(prev);
            d.cam_blend = 0.;
            d.cam_mode = Some(cur);
        }
        let dur = cur.transition_time.max(1e-3);
        d.cam_blend = (d.cam_blend + h / dur).min(1.);
        if d.cam_blend >= 1. { d.cam_from = None; }
        if target.is_none() && d.cam_from.is_none() { d.cam_mode = None; }
    }
    pub fn aerial_anim(&self) -> Option<fn_animgraph::AerialAnim> {
        let c = self.controller.component();
        if !c.is_skydiving() && !c.in_aerial_mode() { return None; }
        let y = self.yaw.to_radians();
        let (f, r) = (Vec3::new(y.sin(), -y.cos(), 0.), Vec3::new(y.cos(), y.sin(), 0.));
        let local = |v: Vec3| [v.dot(f), v.dot(r), v.z];
        let a = c.air.accel_snapshot;
        Some(fn_animgraph::AerialAnim { skydiving: c.is_skydiving(), parachute_open: c.is_parachute_open(), from_launch_pad: c.air.from_launch_pad, local_acceleration: local(a), local_velocity: local(c.velocity) })
    }
    pub fn drop_tick_mode(&self) -> Option<&'static Mode> { self.drop.as_ref().and_then(|d| d.cam_mode) }
    pub(crate) fn drop_compose_ref(&self, st: &camera::CamState, feet: Vec3, world: &impl Fn(Vec3, Vec3, f32) -> Option<f32>) -> Option<(View, f32)> {
        let d = self.drop.as_ref()?;
        if d.in_aircraft {
            let (bus, bus_yaw) = d.aircraft_view()?;
            return Some((aircraft_view(bus, bus_yaw, self.yaw, self.pitch), AIRCRAFT_CAMERA_FOV));
        }
        let cur = d.cam_mode?;
        let mut v = camera::Camera::compose(cur, st, feet, self.yaw, self.pitch, world);
        let mut fov = cur.fov;
        if let Some(from) = d.cam_from {
            let w = ease_in_out(d.cam_blend);
            let f = camera::Camera::compose(from, st, feet, self.yaw, self.pitch, world);
            v.eye = f.eye.lerp(v.eye, w);
            fov = from.fov + (cur.fov - from.fov) * w;
        }
        Some((v, fov))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FrameInput;
    #[test] fn the_flight_path_crosses_the_island_at_the_cooked_height_and_speed() {
        for i in 0..24 {
            let p = initialize_flight_path(i as f32 / 24., (i * 7 % 24) as f32 / 23.);
            assert!((p.start.z - 81312.).abs() < 1e-3);
            let len = p.duration * p.speed;
            assert!(len > 300000. && len < 460000., "{i}: length {len}");
            assert!(p.duration > 50. && p.duration < 77., "{i}: {} s", p.duration);
            assert!(0. <= p.drop_start_offset && p.drop_start_offset < p.drop_end_offset && p.drop_end_offset <= p.duration, "{i}: {p:?}");
            let c = Vec2::from(ISLAND_VOLUME_LOCATION);
            assert!((p.enter - c).length() < 160000. && (p.exit - c).length() < 160000.);
        }
    }
    #[test] fn a_match_on_the_bus_jumps_skydives_glides_and_lands_without_damage() {
        let mut g = GameSim::new();
        g.world.props.clear();
        g.start_match_on_bus(0.25, 0.5);
        assert!(g.on_bus());
        assert_eq!(g.aerial_phase(), AerialPhase::BusCantExit);
        let dt = 1. / 30.;
        let mut n = 0;
        let unlock = g.drop.as_ref().unwrap().aircraft.as_ref().unwrap().drop_start_time();
        while g.drop.as_ref().unwrap().now < unlock - 0.5 { g.step(&FrameInput { jump: n % 2 == 0, ..Default::default() }, dt); n += 1; assert!(g.on_bus() && n < 3000); }
        while g.aerial_phase() == AerialPhase::BusCantExit { g.step(&FrameInput::default(), dt); }
        assert_eq!(g.aerial_phase(), AerialPhase::BusCanExit);
        g.step(&FrameInput::default(), dt);
        g.step(&FrameInput { jump: true, ..Default::default() }, dt);
        assert!(!g.on_bus());
        assert_eq!(g.aerial_phase(), AerialPhase::Skydiving);
        let start_y = g.controller.height;
        for _ in 0..30 { g.step(&FrameInput::default(), dt); }
        assert!(g.controller.height < start_y && g.controller.airborne());
        let mut landed = false;
        for i in 0..(200 * 30) {
            g.step(&FrameInput { forward: 1., pitch_delta: if i == 0 { -40. } else { 0. }, ..Default::default() }, dt);
            if g.controller.grounded() { landed = true; break; }
        }
        assert!(landed, "landed at {}", g.controller.height);
        assert!(g.fall.take_landings().is_empty(), "the first landing after the bus takes no fall damage");
        let ev = g.drop.as_mut().unwrap().take_events();
        assert!(ev.contains(&DropEvent::ParachuteOpened) && ev.contains(&DropEvent::Landed), "{ev:?}");
        assert_eq!(g.controller.component().aerial_mode(), Aerial::None);
    }
    #[test] fn the_match_starts_on_the_spawn_island_and_the_bus_takes_over_after_the_countdown() {
        let mut g = GameSim::new();
        g.world.props.clear();
        g.start_match_on_spawn_island(0.5, 0.3, 0.7, WARMUP_COUNTDOWN_DURATION);
        assert!(g.in_warmup() && !g.on_bus());
        assert!(!g.damage_enabled());
        let o = Vec3::from(SPAWN_ISLAND_LOCATION);
        assert!((g.x - (WARMUP_STARTS[55][0] + o.x) * 0.01).abs() < 1.);
        let dt = 1. / 30.;
        assert_eq!(g.drop.as_ref().unwrap().game_phase_step().0, GamePhaseStep::Warmup);
        let mut t = 0.;
        let mut ready = false;
        while !g.on_bus() { g.step(&FrameInput { forward: 1., ..Default::default() }, dt); t += dt; ready |= g.drop.as_ref().unwrap().game_phase_step().0 == GamePhaseStep::GetReady; assert!(t < WARMUP_COUNTDOWN_DURATION + 1.); }
        assert!(ready && (t - WARMUP_COUNTDOWN_DURATION).abs() < 0.1, "bus at {t}");
        assert_eq!(g.drop.as_ref().unwrap().game_phase_step().0, GamePhaseStep::BusLocked);
        assert!(g.damage_enabled());
        assert!(g.y > 700., "bus height {}", g.y);
    }
    #[test] fn the_bus_auto_ejects_at_the_end_of_the_drop_zone() {
        let mut g = GameSim::new();
        g.world.props.clear();
        g.start_match_on_bus(0.6, 0.2);
        let end = g.drop.as_ref().unwrap().aircraft.as_ref().unwrap().drop_end_time();
        let dt = 1. / 30.;
        let mut t = 0.;
        while g.on_bus() { g.step(&FrameInput::default(), dt); t += dt; assert!(t < end + 1.); }
        assert!((t - end).abs() < 0.1, "ejected at {t}, drop end {end}");
        assert!(g.drop.as_ref().unwrap().events.iter().any(|e| *e == DropEvent::ExitedAircraft { auto: true }));
        assert_eq!(g.drop.as_ref().unwrap().phase, GamePhase::SafeZones);
    }
}
