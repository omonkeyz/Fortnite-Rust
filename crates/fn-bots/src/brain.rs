use crate::bb::{Blackboard, Val};
use crate::bt::{Graph, Host, Runner, Status};
use crate::data::Phoebe;
use crate::skill::{Digest, HitTiming, WeaponClass};
use glam::{Vec2, Vec3};
use std::collections::HashMap;
use std::sync::Arc;

pub const ERROR: i32 = 0;
pub const DENIED: i32 = 1;
pub const SUCCESS: i32 = 2;
pub const PENDING: i32 = 3;
pub const ALLOWED: i32 = 4;

pub const MOVE_NONE: i32 = 0;
pub const MOVE_IN_PROGRESS: i32 = 1;
pub const MOVE_NO_PATH: i32 = 2;
pub const MOVE_ABORTED: i32 = 3;
pub const MOVE_BLOCKED: i32 = 6;
pub const MOVE_SUCCESS: i32 = 9;

pub const ALERT_UNAWARE: i32 = 0;
pub const ALERT_SUSPICIOUS: i32 = 1;
pub const ALERT_LOST: i32 = 2;
pub const ALERT_THREATENED: i32 = 3;

#[derive(Clone, Debug, Default)]
pub struct Agent { pub id: u64, pub team: u32, pub feet: Vec3, pub vel: Vec3, pub alive: bool, pub knocked: bool, pub is_bot: bool, pub airborne: bool, pub hidden: bool, pub health: f32, pub shield: f32 }

#[derive(Clone, Debug)]
pub struct Loot { pub id: u64, pub pos: Vec3, pub kind: i32, pub weapon: bool }

#[derive(Clone, Debug)]
pub struct Poi { pub center: Vec3, pub radius: f32, pub name: String }

#[derive(Clone, Copy, Debug)]
pub struct Storm { pub center: Vec3, pub radius: f32, pub next_center: Vec3, pub next_radius: f32, pub state: i32, pub phase: i32 }

#[derive(Clone, Copy, Debug)]
pub struct Bus { pub pos: Vec3, pub dir: Vec3, pub can_jump: bool, pub time_left: f32 }

#[derive(Clone, Copy, Debug)]
pub struct Sound { pub pos: Vec3, pub loudness: f32, pub instigator: u64, pub team: u32 }

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Body { #[default] Island, Bus, Skydive, Parachute, Ground }

pub trait World {
    fn now(&self) -> f32;
    fn game_phase(&self) -> (i32, i32);
    fn agents(&self) -> &[Agent];
    fn los(&self, from: Vec3, to: Vec3) -> bool;
    fn pois(&self) -> &[Poi];
    fn loot_near(&self, p: Vec3, r: f32, out: &mut Vec<Loot>);
    fn storm(&self) -> Option<Storm>;
    fn bus(&self) -> Option<Bus>;
    fn sounds(&self) -> &[Sound];
    fn poi_occupancy(&self, poi: usize) -> u32;
    fn find_path(&self, from: Vec3, to: Vec3) -> Option<Vec<Vec3>>;
    fn random_point(&self, center: Vec3, r: f32, seed: u64) -> Option<Vec3>;
}

#[derive(Clone, Debug)]
pub struct Weapon { pub row: String, pub class: WeaponClass, pub clip: u32, pub ammo: u32, pub fire_interval: f32, pub reload_time: f32, pub range_m: f32 }

#[derive(Clone, Debug, Default)]
pub struct Intent {
    pub move_dir: Vec2,
    pub sprint: bool,
    pub crouch: bool,
    pub jump: bool,
    pub aim_yaw: f32,
    pub aim_pitch: f32,
    pub strafe: bool,
    pub fire: Option<FireRequest>,
    pub ads: bool,
    pub weapon: Option<usize>,
    pub interact: Option<u64>,
    pub exit_bus: bool,
    pub aerial: Vec2,
    pub aerial_dive: bool,
    pub reloading: bool,
    pub emote: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
pub struct FireRequest { pub weapon: usize, pub dir: Vec3, pub hit: bool, pub target: u64 }

#[derive(Clone, Copy, Debug, Default)]
struct Seen { first: f32, last_visible: f32, perceived: bool, visible: bool, reaction: f32, lose: f32, pos: Vec3 }

#[derive(Clone, Debug, Default)]
pub struct Nav { pub goal: Option<Vec3>, pub radius: f32, pub path: Vec<Vec3>, pub idx: usize, pub started: f32, pub progress_t: f32, pub progress_pos: Vec3, pub result: i32, pub direct: bool, pub replan_at: f32, pub plans: u32, pub best: f32 }

pub struct Core {
    pub id: u64,
    pub team: u32,
    pub name: String,
    pub bb: Blackboard,
    pub skill: Digest,
    pub data: Arc<Phoebe>,
    pub rng: u64,
    pub now: f32,
    pub feet: Vec3,
    pub vel: Vec3,
    pub eye_h: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub body: Body,
    pub alive: bool,
    pub health: f32,
    pub shield: f32,
    pub intent: Intent,
    pub nav: Nav,
    seen: HashMap<u64, Seen>,
    pub target: Option<u64>,
    target_since: f32,
    loudness: f32,
    loud_pos: Vec3,
    pub weapons: Vec<Weapon>,
    pub equipped: Option<usize>,
    pub reload_until: f32,
    next_shot: f32,
    next_hit: f32,
    hit_timing: Option<(WeaponClass, HitTiming)>,
    pattern_t: f32,
    pub poi: Option<usize>,
    poi_stay_until: f32,
    poi_fail: u32,
    visited: Vec<usize>,
    landing: Option<Vec3>,
    jump_decided: bool,
    pub follow: bool,
    pub exit_at: Option<f32>,
    pub interact_done: Option<u64>,
    pub looted: Vec<u64>,
    loot_target: Option<Loot>,
    interacting: Option<(u64, f32)>,
    patrol_center: Option<Vec3>,
    wait_until: HashMap<u32, f32>,
    crouch_req: Option<bool>,
    jump_pulse: bool,
    evasive_next: f32,
    emote_cooldown: f32,
    next_sight: f32,
    emote_until: f32,
    pub damaged_by: Vec<(u64, Vec3)>,
    pub last_kill: Option<(Vec3, f32)>,
    pub trace: Vec<String>,
    scratch: Vec<Loot>,
    pub stats: Stats,
}

#[derive(Clone, Debug, Default)]
pub struct Stats { pub shots: u32, pub hits_planned: u32, pub loot_interactions: u32, pub jumped_at: Option<f32>, pub landed_at: Option<f32> }

pub struct Bot { pub runner: Runner, pub core: Core }

pub struct Ctx<'a> { pub c: &'a mut Core, pub w: &'a dyn World }

fn hash(mut x: u64) -> u64 { x ^= x >> 33; x = x.wrapping_mul(0xff51afd7ed558ccd); x ^= x >> 33; x = x.wrapping_mul(0xc4ceb9fe1a85ec53); x ^ (x >> 33) }

impl Core {
    pub fn rand(&mut self) -> f32 { self.rng = hash(self.rng.wrapping_add(0x9e3779b97f4a7c15)); (self.rng >> 40) as f32 / (1u64 << 24) as f32 }
    pub fn eye(&self) -> Vec3 { self.feet + Vec3::Y * self.eye_h }
    fn dev(&mut self, v: f32, d: f32) -> f32 { (v + (self.rand() * 2. - 1.) * d).max(0.) }
    pub fn forward(&self) -> Vec3 { let (y, p) = (self.yaw.to_radians(), self.pitch.to_radians()); Vec3::new(y.sin() * p.cos(), p.sin(), -y.cos() * p.cos()) }
    fn has_gun(&self) -> bool { self.weapons.iter().any(|w| w.class != WeaponClass::Melee) }
    pub fn landing(&self) -> Option<Vec3> { self.landing }
    pub fn set_landing(&mut self, p: Vec3, poi: Option<usize>) { self.landing = Some(p); if poi.is_some() { self.poi = poi; } }
}

pub fn yaw_to(d: Vec3) -> f32 { d.x.atan2(-d.z).to_degrees().rem_euclid(360.) }
pub fn pitch_to(d: Vec3) -> f32 { let h = Vec2::new(d.x, d.z).length(); d.y.atan2(h).to_degrees() }

impl Bot {
    pub fn new(data: Arc<Phoebe>, g: &Graph, id: u64, team: u32, name: String, level: f32, seed: u64) -> Bot {
        let bb = Blackboard::new(g.schema.clone());
        let skill = Digest::new(&data, level);
        let core = Core { id, team, name, bb, skill, data, rng: hash(seed ^ 0xb07), now: 0., feet: Vec3::ZERO, vel: Vec3::ZERO, eye_h: 1.55, yaw: 0., pitch: 0., body: Body::Island, alive: true, health: 100., shield: 0.,
            intent: Intent::default(), nav: Nav::default(), seen: HashMap::new(), target: None, target_since: 0., loudness: 0., loud_pos: Vec3::ZERO, weapons: vec![], equipped: None, reload_until: 0., next_shot: 0., next_hit: 0., hit_timing: None, pattern_t: 0.,
            poi: None, poi_stay_until: 0., poi_fail: 0, visited: vec![], landing: None, jump_decided: false, follow: false, exit_at: None, interact_done: None, looted: vec![], loot_target: None, interacting: None, patrol_center: None, wait_until: HashMap::new(), crouch_req: None, jump_pulse: false, evasive_next: 0., emote_cooldown: 0., emote_until: 0., next_sight: 0.,
            damaged_by: vec![], last_kill: None, trace: vec![], scratch: vec![], stats: Stats::default() };
        Bot { runner: Runner::new(g), core }
    }

    pub fn think(&mut self, g: &Graph, w: &dyn World, dt: f32) {
        let c = &mut self.core;
        c.now = w.now();
        c.intent.jump = false;
        c.intent.exit_bus = false;
        if !matches!(c.body, Body::Ground | Body::Island) { c.seen.clear(); c.target = None; c.bb.set_bool("AIEvaluator_RangeAttack_IsReadyToFire", false); c.bb.set_bool("AIEvaluator_ManageWeapon_Fire", false); }
        c.intent.fire = None;
        if !c.alive { return; }
        let mut ctx = Ctx { c, w };
        ctx.globals();
        ctx.perceive(dt);
        self.runner.tick(g, &mut ctx, dt);
        ctx.post(dt);
    }
}

impl<'a> Ctx<'a> {
    fn d(&self) -> &Phoebe { &self.c.data }

    fn globals(&mut self) {
        let (phase, step) = self.w.game_phase();
        let c = &mut *self.c;
        c.bb.set_int("AIEvaluator_Global_GamePhase", phase);
        c.bb.set_int("AIEvaluator_Global_GamePhaseStep", step);
        c.bb.set_bool("AIEvaluator_Global_IsInBus", c.body == Body::Bus);
        let jumped = matches!(c.body, Body::Skydive | Body::Parachute) || (c.body == Body::Ground && c.stats.jumped_at.is_some());
        if jumped { c.bb.set_bool("AIEvaluator_Global_HasEverJumpedFromBusKey", true); }
        if c.body == Body::Ground && c.stats.jumped_at.is_some() { c.bb.set_bool("AIEvaluator_Global_HasEverJumpedFromBusAndLandedKey", true); }
        c.bb.set_bool("AIEvaluator_Global_DBNO", false);
        c.bb.set_bool("AIEvaluator_Global_IsMovementBlocked", false);
        c.bb.set_bool("AIEvaluator_Global_UseEndGameBehavior", false);
        c.bb.set_obj("SelfActor", Some(c.id));
    }

    fn agent(&self, id: u64) -> Option<&Agent> { self.w.agents().iter().find(|a| a.id == id) }

    fn perceive(&mut self, dt: f32) {
        let now = self.c.now;
        let alert = self.c.bb.int("AIEvaluator_Global_AlertLevel");
        let a = if alert >= ALERT_SUSPICIOUS { 1 } else { 0 };
        let sk = self.c.skill.clone();
        let eye = self.c.eye();
        let fwd = { let f = self.c.forward(); Vec3::new(f.x, 0., f.z).normalize_or_zero() };
        let pov = eye - fwd * sk.backward_cm[a] * 0.01;
        let cos_half = sk.half_angle[a].to_radians().cos();
        let in_ground = matches!(self.c.body, Body::Ground | Body::Island);
        let sight_due = now >= self.c.next_sight;
        if sight_due { let iv = self.c.dev(sk.targeting_interval, sk.targeting_dev).clamp(0.1, 0.6); self.c.next_sight = now + iv; }
        let mut ids = vec![];
        for ag in self.w.agents() {
            if ag.id == self.c.id || !ag.alive || ag.hidden || ag.team == self.c.team { continue; }
            ids.push((ag.id, ag.feet + Vec3::Y * 1.2));
        }
        for (id, p) in ids {
            let d = p.distance(eye);
            let s = self.c.seen.get(&id).copied().unwrap_or_default();
            let radius = if s.perceived { sk.lose_sight_radius[a] } else { sk.sight_radius[a] } * 0.01;
            let mut visible = false;
            if !sight_due { visible = s.visible && in_ground && d <= radius; }
            else if in_ground && d <= radius {
                let to = (p - pov).normalize_or_zero();
                let ang_ok = d <= sk.near_clip_cm[a] * 0.01 || Vec3::new(to.x, 0., to.z).normalize_or_zero().dot(fwd) >= cos_half || s.perceived;
                if ang_ok { visible = self.w.los(eye, p); }
            }
            let mut s = s;
            if visible {
                if !s.visible { s.first = now; let add = self.d().row("Phoebe_DistanceSkillSetModifiers", "Phoebe.Skill.Perception.SightRandomAddition", d * 100.).unwrap_or(0.) * self.c.rand(); s.reaction = self.c.dev(sk.sight_reaction, sk.sight_dev) + add; }
                s.visible = true; s.last_visible = now; s.pos = p;
                if now - s.first >= s.reaction { s.perceived = true; s.lose = self.c.dev(sk.lose_sight, sk.lose_sight_dev); }
            } else {
                s.visible = false;
                if s.perceived && now - s.last_visible > s.lose { s.perceived = false; }
            }
            if s.visible || s.perceived || self.c.seen.contains_key(&id) { self.c.seen.insert(id, s); }
        }
        for (id, pos) in std::mem::take(&mut self.c.damaged_by) {
            let s = self.c.seen.entry(id).or_default();
            s.perceived = true; s.last_visible = now; s.pos = pos; s.lose = sk.lose_sight.max(1.); s.first = now - 10.;
        }
        let team = self.c.team;
        for snd in self.w.sounds() {
            if snd.team == team || snd.instigator == self.c.id { continue; }
            let d = snd.pos.distance(eye) * 100.;
            let range = 30000_f32.max(1.);
            if d > range { continue; }
            self.c.loudness += snd.loudness * (1. - d / range);
            let err = sk.hearing_error_cm * 0.01;
            let (ex, ez) = (self.c.rand() * 2. - 1., self.c.rand() * 2. - 1.);
            self.c.loud_pos = snd.pos + Vec3::new(ex * err, 0., ez * err);
        }
        self.c.loudness = (self.c.loudness - 25. * dt).max(0.);
        let best = self.c.seen.iter().filter(|(id, s)| s.perceived && self.agent(**id).map_or(false, |a| a.alive)).map(|(id, s)| (*id, s.pos.distance(eye))).min_by(|a, b| a.1.total_cmp(&b.1));
        let prev = self.c.bb.int("AIEvaluator_Global_AlertLevel");
        let new_alert;
        if let Some((id, _)) = best {
            if self.c.target != Some(id) { self.c.target = Some(id); self.c.target_since = now; }
            let s = self.c.seen[&id];
            self.c.bb.set_obj("AIEvaluator_Global_TargetActor", Some(id));
            self.c.bb.set_bool("AIEvaluator_Global_HasLoSOnThreat", s.visible);
            self.c.bb.set_vec("AIEvaluator_Global_LastKnownPosition", Some(s.pos));
            self.c.bb.set_bool("IsInCombat", true);
            new_alert = ALERT_THREATENED;
        } else {
            let had = self.c.target.take().is_some();
            self.c.bb.set_obj("AIEvaluator_Global_TargetActor", None);
            self.c.bb.set_bool("AIEvaluator_Global_HasLoSOnThreat", false);
            self.c.bb.set_bool("IsInCombat", false);
            if had { new_alert = ALERT_LOST; }
            else if self.c.loudness >= sk.lkp_loudness { new_alert = ALERT_LOST.max(prev.min(ALERT_LOST)); self.c.bb.set_vec("AIEvaluator_Global_LastKnownPosition", Some(self.c.loud_pos)); }
            else if self.c.loudness >= sk.alerted_loudness { new_alert = prev.clamp(ALERT_SUSPICIOUS, ALERT_LOST); self.c.bb.set_vec("AIEvaluator_Global_LastKnownPosition", Some(self.c.loud_pos)); }
            else { new_alert = if prev == ALERT_THREATENED { ALERT_LOST } else { prev }; }
        }
        self.c.bb.set_int("AIEvaluator_Global_AlertLevel", new_alert);
        self.c.seen.retain(|_, s| s.perceived || s.visible || now - s.last_visible < 10.);
    }

    fn post(&mut self, dt: f32) {
        let c = &mut *self.c;
        if let Some(r) = c.crouch_req { c.intent.crouch = r; }
        if c.jump_pulse { c.intent.jump = true; c.jump_pulse = false; }
        let focal = c.bb.vector("AIEvaluator_Global_FocalPoint");
        let want = match focal { Some(f) => { let d = f - c.eye(); Some((yaw_to(d), pitch_to(d))) } None => None };
        let rate = 1. / c.skill.tracking_interp.max(0.02);
        match want {
            Some((y, p)) => {
                let k = (1. - (-rate * dt).exp()).clamp(0., 1.);
                let dy = ((y - c.yaw + 540.).rem_euclid(360.)) - 180.;
                c.yaw = (c.yaw + dy * k).rem_euclid(360.);
                c.pitch += (p - c.pitch) * k;
                c.intent.strafe = true;
            }
            None => {
                if c.intent.move_dir.length_squared() > 0.01 { let y = yaw_to(Vec3::new(c.intent.move_dir.x, 0., c.intent.move_dir.y)); let dy = ((y - c.yaw + 540.).rem_euclid(360.)) - 180.; c.yaw = (c.yaw + dy * (1. - (-8. * dt).exp())).rem_euclid(360.); }
                c.pitch *= 1. - (1. - (-4. * dt).exp());
                c.intent.strafe = false;
            }
        }
        c.intent.aim_yaw = c.yaw;
        c.intent.aim_pitch = c.pitch;
        c.intent.weapon = c.equipped;
        c.intent.reloading = c.now < c.reload_until;
    }

    pub fn steer(&mut self) -> Vec2 {
        let Some(goal) = self.c.nav.goal else { return Vec2::ZERO };
        let far = Vec2::new(goal.x - self.c.feet.x, goal.z - self.c.feet.z).length() > 2.;
        if !self.c.nav.direct && far && self.c.nav.idx >= self.c.nav.path.len() && self.c.now >= self.c.nav.replan_at {
            let p = self.w.find_path(self.c.feet, goal);
            let wait = 0.5 + self.c.rand();
            let c = &mut *self.c;
            c.nav.plans += 1;
            match p { Some(p) if !p.is_empty() => { c.nav.path = p; c.nav.idx = 0; c.nav.replan_at = c.now + 0.25; } _ => { c.nav.path.clear(); c.nav.idx = 0; c.nav.replan_at = c.now + wait; } }
        }
        let c = &mut *self.c;
        let p = c.feet;
        while c.nav.idx < c.nav.path.len() && Vec2::new(c.nav.path[c.nav.idx].x - p.x, c.nav.path[c.nav.idx].z - p.z).length() < 0.6 { c.nav.idx += 1; }
        let wp = c.nav.path.get(c.nav.idx).copied().unwrap_or(goal);
        let d = Vec2::new(wp.x - p.x, wp.z - p.z);
        if d.length_squared() < 1e-4 { Vec2::ZERO } else { d.normalize() }
    }

    fn set_goal(&mut self, goal: Vec3, radius: f32, direct: bool) {
        let now = self.c.now;
        let same = self.c.nav.goal.map_or(false, |g| g.distance(goal) < 0.5);
        if same { return; }
        let c = &mut *self.c;
        c.nav = Nav { goal: Some(goal), radius, path: vec![], idx: 0, started: now, progress_t: now, progress_pos: c.feet, result: MOVE_IN_PROGRESS, direct, replan_at: now, plans: 0, best: f32::MAX };
    }

    fn move_status(&mut self) -> i32 {
        let c = &mut *self.c;
        let Some(goal) = c.nav.goal else { return MOVE_NONE };
        let flat = Vec2::new(goal.x - c.feet.x, goal.z - c.feet.z).length();
        if flat <= c.nav.radius.max(0.3) && (goal.y - c.feet.y).abs() < 3. { c.nav.result = MOVE_SUCCESS; return MOVE_SUCCESS; }
        let d3 = flat + (goal.y - c.feet.y).abs() * 0.5;
        if d3 < c.nav.best - 0.5 { c.nav.best = d3; c.nav.progress_pos = c.feet; c.nav.progress_t = c.now; }
        let stuck = c.now - c.nav.progress_t;
        if stuck > 1.2 && stuck < 1.4 { c.jump_pulse = true; }
        if stuck > 4.0 { c.nav.result = MOVE_BLOCKED; return MOVE_BLOCKED; }
        MOVE_IN_PROGRESS
    }

    fn stop_move(&mut self) { self.c.nav.goal = None; self.c.intent.move_dir = Vec2::ZERO; self.c.intent.sprint = false; }

    fn status(&mut self, key: &str, v: i32) { self.c.bb.set_int(key, v); }

    fn key_location(&self, g: &Graph, k: u16) -> Option<Vec3> {
        let _ = g;
        match self.c.bb.get(k) { Val::Vec(v) => v, Val::Obj(Some(id)) => self.agent(id).map(|a| a.feet), _ => None }
    }

    fn evaluator(&mut self, class: &str, props: Option<&serde_json::Value>) {
        match class {
            "FortAthenaAIBotEvaluator_JumpOffBus" => self.ev_jump_off_bus(),
            "FortAthenaAIBotEvaluator_FreeFalling" => self.ev_free_fall(),
            "FortAthenaAIBotEvaluator_Warmup" => self.ev_warmup(),
            "FortAthenaAIBotEvaluator_PlayEmote" => self.ev_play_emote(),
            "FortAthenaAIBotEvaluator_Loot" => self.ev_loot(),
            "FortAthenaAIBotEvaluator_Sprinting" => self.ev_sprinting(props),
            "FortAthenaAIBotEvaluator_SelectNextDynamicPOI" => self.ev_next_poi(true),
            "FortAthenaAIBotEvaluator_SelectNextPOI" => self.ev_next_poi(false),
            "FortAthenaAIBotEvaluator_Storm" => self.ev_storm(),
            "FortAthenaAIBotEvaluator_PatrolAround" => self.ev_patrol(),
            "FortAthenaAIBotEvaluator_WeaponSelection" => self.ev_weapon_selection(),
            "FortAthenaAIBotEvaluator_RangeAttack" => self.ev_range_attack(props),
            "FortAthenaAIBotEvaluator_MeleeAttack" => self.ev_melee(),
            "FortAthenaAIBotEvaluator_HandleFocusing" => self.ev_focus(props),
            "FortAthenaAIBotEvaluator_AimDownSight" => self.ev_ads(props),
            "FortAthenaAIBotEvaluator_Investigate" => self.ev_investigate(),
            "FortAthenaAIBotEvaluator_EvasiveManeuvers" => self.ev_evasive(props),
            other => self.ev_denied(other),
        }
    }

    fn ev_denied(&mut self, class: &str) {
        for key in denied_keys(class) { self.status(key, DENIED); }
    }

    fn ev_jump_off_bus(&mut self) {
        if self.c.body != Body::Bus { self.status("AIEvaluator_JumpOffBus_ExecutionStatus", DENIED); return; }
        let Some(bus) = self.w.bus() else { return };
        if self.c.landing.is_none() {
            if let Some(i) = self.pick_poi(None) {
                self.c.poi = Some(i);
                let p = &self.w.pois()[i];
                let (c, r) = (p.center, p.radius * 0.7);
                let seed = self.c.rng;
                let land = self.w.random_point(c, r, seed).unwrap_or(c);
                self.c.landing = Some(land);
            }
        }
        let Some(dest) = self.c.landing else { return };
        self.c.bb.set_vec("AIEvaluator_JumpOffBus_Destination", Some(dest));
        if !bus.can_jump { self.status("AIEvaluator_JumpOffBus_ExecutionStatus", DENIED); return; }
        let to = Vec2::new(dest.x - bus.pos.x, dest.z - bus.pos.z);
        let f = Vec2::new(bus.dir.x, bus.dir.z).normalize_or_zero();
        let ang = f.angle_to(to.normalize_or_zero()).to_degrees().abs();
        let (lo, hi) = self.c.skill.jump_off_angle;
        let chance = self.d().row("Phoebe_SelectPOIToJumpChanceByDistance", "", to.length() * 100.).unwrap_or(1.);
        let go = if self.c.follow { self.c.exit_at.map_or(false, |t| self.c.now >= t) || bus.time_left < 2. } else { (ang >= lo && ang <= hi && self.c.rand() <= chance) || ang > hi || to.length() < 250. || bus.time_left < 2. };
        self.status("AIEvaluator_JumpOffBus_ExecutionStatus", if go { ALLOWED } else { PENDING });
    }

    fn ev_free_fall(&mut self) {
        let dest = self.c.landing.or_else(|| self.c.poi.map(|i| self.w.pois()[i].center));
        self.c.bb.set_vec("AIEvaluator_Dive_Destination", dest);
        self.c.bb.set_vec("AIEvaluator_Glide_Destination", dest);
        let (dive, glide) = match self.c.body { Body::Skydive => (ALLOWED, DENIED), Body::Parachute => (DENIED, ALLOWED), _ => (DENIED, DENIED) };
        self.status("AIEvaluator_Dive_ExecutionStatus", dive);
        self.status("AIEvaluator_Glide_ExecutionStatus", glide);
    }

    fn ev_warmup(&mut self) {
        self.status("AIEvaluator_WarmupIdle_ExecutionStatus", DENIED);
        if self.c.body != Body::Island { self.status("AIEvaluator_WarmupPlayEmote_ExecutionStatus", DENIED); self.status("AIEvaluator_WarmupLootAndShoot_ExecutionStatus", DENIED); return; }
        let mut list = std::mem::take(&mut self.c.scratch);
        list.clear();
        self.w.loot_near(self.c.feet, self.c.skill.loot_radius_cm * 0.01, &mut list);
        let loot = list.iter().any(|l| !self.c.looted.contains(&l.id));
        self.c.scratch = list;
        let threatened = self.c.bb.int("AIEvaluator_Global_AlertLevel") == ALERT_THREATENED;
        let emoting = self.c.bb.int("AIEvaluator_PlayEmote_ExecutionStatus") >= PENDING;
        let shoot = loot || threatened && !emoting && self.c.has_gun();
        self.status("AIEvaluator_WarmupLootAndShoot_ExecutionStatus", if shoot { PENDING } else { DENIED });
        self.status("AIEvaluator_WarmupPlayEmote_ExecutionStatus", if shoot { DENIED } else { PENDING });
    }

    fn ev_play_emote(&mut self) {
        let warm = self.c.bb.int("AIEvaluator_WarmupPlayEmote_ExecutionStatus") >= PENDING && self.c.body == Body::Island;
        if !warm || self.c.now < self.c.emote_cooldown { self.status("AIEvaluator_PlayEmote_ExecutionStatus", DENIED); return; }
        if self.c.bb.vector("AIEvaluator_PlayEmote_Destination").is_none() {
            let r = 8. + 17. * self.c.rand();
            let a = self.c.rand() * std::f32::consts::TAU;
            let seed = self.c.rng;
            let p = self.c.feet + Vec3::new(a.cos() * r, 0., a.sin() * r);
            let p = self.w.random_point(p, 0., seed).unwrap_or(p);
            self.c.bb.set_vec("AIEvaluator_PlayEmote_Destination", Some(p));
        }
        self.status("AIEvaluator_PlayEmote_ExecutionStatus", PENDING);
    }

    fn ev_loot(&mut self) {
        let sk = self.c.skill.clone();
        let feet = self.c.feet;
        if let Some(done) = self.c.interact_done.take() { if !self.c.looted.contains(&done) { self.c.looted.push(done); } self.c.loot_target = None; self.c.interacting = None; self.status("AIEvaluator_Loot_InteractionExecutionStatus", SUCCESS); }
        let mut list = std::mem::take(&mut self.c.scratch);
        list.clear();
        self.w.loot_near(feet, sk.loot_radius_cm * 0.01, &mut list);
        let no_gun = !self.c.has_gun();
        let best = list.iter().filter(|l| !self.c.looted.contains(&l.id)).map(|l| {
            let d2 = Vec2::new(l.pos.x - feet.x, l.pos.z - feet.z).length() * 100.;
            let dh = (l.pos.y - feet.y).abs() * 100.;
            let score = sk.loot_dist_score * d2 + sk.loot_height_score * (dh / 100.).floor() + if no_gun && l.weapon { sk.loot_weapon_score } else { 0. };
            (l.clone(), score)
        }).max_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0);
        self.c.scratch = list;
        match best {
            Some(l) => {
                self.c.bb.set_vec("AIEvaluator_Loot_Destination", Some(l.pos));
                self.c.bb.set_obj("AIEvaluator_Loot_Object", Some(l.id));
                self.c.bb.set_obj("AIEvaluator_Loot_InteractionContextInfo", Some(l.id));
                self.c.bb.set_int("AIEvaluator_Loot_Type", l.kind);
                let near = Vec2::new(l.pos.x - feet.x, l.pos.z - feet.z).length() < 2.2 && (l.pos.y - feet.y).abs() < 2.;
                self.status("AIEvaluator_Loot_InteractionExecutionStatus", if near { PENDING } else { DENIED });
                self.status("AIEvaluator_Loot_ExecutionStatus", PENDING);
                if self.c.bb.int("AIEvaluator_Loot_MovementState") == MOVE_BLOCKED { self.c.looted.push(l.id); self.c.bb.set_int("AIEvaluator_Loot_MovementState", MOVE_NONE); }
                self.c.loot_target = Some(l);
            }
            None => {
                self.c.bb.set_vec("AIEvaluator_Loot_Destination", None);
                self.c.bb.set_obj("AIEvaluator_Loot_Object", None);
                self.status("AIEvaluator_Loot_InteractionExecutionStatus", DENIED);
                self.status("AIEvaluator_Loot_ExecutionStatus", DENIED);
                self.c.loot_target = None;
            }
        }
    }

    fn ev_sprinting(&mut self, props: Option<&serde_json::Value>) {
        let key = props.and_then(|p| p.get("AllowSprintKeyName")).and_then(|v| v.as_str()).unwrap_or("AIEvaluator_Sprinting_AllowSprint").to_string();
        let far = self.c.nav.goal.map_or(false, |g| Vec2::new(g.x - self.c.feet.x, g.z - self.c.feet.z).length() * 100. > self.c.skill.sprint_min_dist_cm);
        let allowed = self.c.bb.k(&key).map_or(true, |k| !matches!(self.c.bb.get(k), Val::Bool(false)) || !self.c.bb.boolean("AIEvaluator_Investigate_Cautious"));
        self.c.bb.set_bool(&key, allowed);
        self.status("AIEvaluator_Sprinting_ExecutionStatus", if far && allowed { ALLOWED } else { DENIED });
    }

    fn pick_poi(&mut self, exclude: Option<usize>) -> Option<usize> {
        let pois = self.w.pois();
        if pois.is_empty() { return None; }
        let d = self.d();
        let fd = d.row("Phoebe_Data", "Phoebe.ServicePlayerBots.DynamicBotPOI.BotPOIScoringDistanceFactor", 1.).unwrap_or(5.);
        let fo = d.row("Phoebe_Data", "Phoebe.ServicePlayerBots.DynamicBotPOI.BotPOIScoringOccupancyFactor", 1.).unwrap_or(10.);
        let fv = d.row("Phoebe_Data", "Phoebe.ServicePlayerBots.DynamicBotPOI.BotPOIScoringVisitedFactor", 1.).unwrap_or(7.);
        let fw = d.row("Phoebe_Data", "Phoebe.ServicePlayerBots.DynamicBotPOI.BotPOIScoringWeightFactor", 1.).unwrap_or(0.);
        let from = if self.c.body == Body::Bus { self.w.bus().map_or(self.c.feet, |b| b.pos) } else { self.c.feet };
        let line = if self.c.body == Body::Bus { self.w.bus().map(|b| Vec2::new(b.dir.x, b.dir.z).normalize_or_zero()) } else { None };
        let dist_of = |c: Vec3| { let d = Vec2::new(c.x - from.x, c.z - from.z); match line { Some(l) if d.dot(l) > -50. => (d - l * d.dot(l)).length(), Some(_) => f32::MAX, None => d.length() } };
        let storm = self.w.storm();
        let maxd = pois.iter().map(|p| dist_of(p.center)).filter(|d| d.is_finite() && *d < f32::MAX).fold(1.0f32, f32::max);
        let maxo = (0..pois.len()).map(|i| self.w.poi_occupancy(i)).max().unwrap_or(0).max(1) as f32;
        let mut best = None;
        for (i, p) in pois.iter().enumerate() {
            if Some(i) == exclude { continue; }
            if let Some(s) = storm { if s.state > 0 && Vec2::new(p.center.x - s.next_center.x, p.center.z - s.next_center.z).length() > s.next_radius { continue; } }
            let raw = dist_of(p.center);
            if raw == f32::MAX { continue; }
            let dist = raw / maxd;
            let occ = self.w.poi_occupancy(i) as f32 / maxo;
            let visited = self.c.visited.contains(&i) as i32 as f32;
            let noise = self.c.rand();
            let score = fd * (1. - dist) + fo * (1. - occ) + fv * (1. - visited) + fw + noise * 2.;
            if best.map_or(true, |(_, s)| score > s) { best = Some((i, score)); }
        }
        best.map(|b| b.0)
    }

    fn ev_next_poi(&mut self, dynamic: bool) {
        let (status_key, loc_key) = if dynamic { ("AIEvaluator_DynamicPOINavigation_ExecutionStatus", "AIEvaluator_DynamicPOINavigation_Location") } else { ("AIEvaluator_POINavigation_ExecutionStatus", "AIEvaluator_Marker_MarkerLocation") };
        let enabled = self.d().row("Phoebe_Data", "Phoebe.ServicePlayerBots.DynamicBotPOI.NavigationWithDynamicPOIEnabled", 1.).unwrap_or(1.) > 0.5;
        if dynamic != enabled { self.status(status_key, DENIED); return; }
        if self.c.body != Body::Ground { self.status(status_key, DENIED); return; }
        let now = self.c.now;
        let at_poi = self.c.poi.map(|i| { let p = &self.w.pois()[i]; Vec2::new(p.center.x - self.c.feet.x, p.center.z - self.c.feet.z).length() < p.radius.max(30.) }).unwrap_or(false);
        if at_poi && self.c.poi_stay_until == 0. {
            let lo = self.d().row("Phoebe_Data", "Phoebe.ServicePlayerBots.DynamicBotPOI.BotPOINextPOIMinDuration", 1.).unwrap_or(20.);
            let hi = self.d().row("Phoebe_Data", "Phoebe.ServicePlayerBots.DynamicBotPOI.BotPOINextPOIMaxDuration", 1.).unwrap_or(180.);
            let u = self.c.rand();
            self.c.poi_stay_until = now + lo + (hi - lo) * u;
            if let Some(i) = self.c.poi { if !self.c.visited.contains(&i) { self.c.visited.push(i); } }
            self.c.patrol_center = self.c.poi.map(|i| self.w.pois()[i].center);
        }
        if self.c.poi.is_none() || (self.c.poi_stay_until > 0. && now > self.c.poi_stay_until) || self.c.nav.result == MOVE_BLOCKED && self.c.bb.int(if dynamic { "AIEvaluator_DynamicPOINavigation_MovementState" } else { "AIEvaluator_POINavigation_MovementState" }) == MOVE_BLOCKED {
            if self.c.nav.result == MOVE_BLOCKED { self.c.poi_fail += 1; }
            let fail_max = self.d().row("Phoebe_Data", "Phoebe.ServicePlayerBots.DynamicBotPOI.FailToReachBotPOICountMax", 1.).unwrap_or(2.) as u32;
            if self.c.poi.is_none() || self.c.poi_stay_until > 0. || self.c.poi_fail >= fail_max {
                let ex = self.c.poi;
                self.c.poi = self.pick_poi(ex);
                self.c.poi_stay_until = 0.;
                self.c.poi_fail = 0;
                self.c.bb.set_int(if dynamic { "AIEvaluator_DynamicPOINavigation_MovementState" } else { "AIEvaluator_POINavigation_MovementState" }, MOVE_NONE);
            }
        }
        let Some(i) = self.c.poi else { self.status(status_key, DENIED); return };
        if self.c.poi_stay_until > 0. { self.status(status_key, DENIED); return; }
        let target = self.w.pois()[i].center;
        self.c.bb.set_vec(loc_key, Some(target));
        self.c.bb.set_obj("AIEvaluator_POINavigation_NextPOI", Some(i as u64));
        self.status(status_key, PENDING);
    }

    fn ev_storm(&mut self) {
        let Some(s) = self.w.storm() else { self.status("AIEvaluator_Storm_ExecutionStatus", DENIED); return };
        self.c.bb.set_int("AIEvaluator_Storm_State", s.state);
        self.c.bb.set_int("AIEvaluator_Storm_Phase", s.phase);
        if self.c.body != Body::Ground || s.state == 0 { self.status("AIEvaluator_Storm_ExecutionStatus", DENIED); return; }
        let f = self.c.feet;
        let out_now = Vec2::new(f.x - s.center.x, f.z - s.center.z).length() > s.radius;
        let out_next = Vec2::new(f.x - s.next_center.x, f.z - s.next_center.z).length() > s.next_radius * 0.9;
        if out_now || out_next {
            if self.c.bb.int("AIEvaluator_Storm_ExecutionStatus") != PENDING || self.c.bb.vector("AIEvaluator_Storm_Destination").is_none() {
                let dir = Vec2::new(s.next_center.x - f.x, s.next_center.z - f.z);
                let d = dir.length();
                let keep = (d - s.next_radius * 0.6).max(0.);
                let to = Vec2::new(f.x, f.z) + dir.normalize_or_zero() * keep;
                self.c.bb.set_vec("AIEvaluator_Storm_Destination", Some(Vec3::new(to.x, f.y, to.y)));
            }
            self.status("AIEvaluator_Storm_ExecutionStatus", PENDING);
        } else {
            self.status("AIEvaluator_Storm_ExecutionStatus", DENIED);
            self.c.bb.set_vec("AIEvaluator_Storm_Destination", None);
        }
    }

    fn ev_patrol(&mut self) {
        if self.c.body != Body::Ground && self.c.body != Body::Island { self.status("AIEvaluator_Patrol_ExecutionStatus", DENIED); return; }
        let need = match self.c.bb.vector("AIEvaluator_Patrol_Destination") { None => true, Some(d) => Vec2::new(d.x - self.c.feet.x, d.z - self.c.feet.z).length() < 2. || self.c.bb.int("AIEvaluator_Patrol_MovementState") == MOVE_BLOCKED };
        if need {
            let center = self.c.patrol_center.unwrap_or(self.c.feet);
            let r = self.c.dev(self.c.skill.patrol_distance_cm, self.c.skill.patrol_dev_cm) * 0.01;
            let r = if self.c.body == Body::Island { r.min(25.) } else { r };
            let seed = self.c.rng;
            let p = self.w.random_point(center, r, seed).unwrap_or_else(|| { let a = self.c.rand() * std::f32::consts::TAU; let k = self.c.rand().sqrt() * r; center + Vec3::new(a.cos() * k, 0., a.sin() * k) });
            self.c.bb.set_vec("AIEvaluator_Patrol_Destination", Some(p));
            self.c.bb.set_int("AIEvaluator_Patrol_MovementState", MOVE_NONE);
        }
        self.status("AIEvaluator_Patrol_ExecutionStatus", SUCCESS);
    }

    fn target_dist(&self) -> Option<f32> { let t = self.c.target?; self.agent(t).map(|a| a.feet.distance(self.c.feet)) }

    fn ev_weapon_selection(&mut self) {
        let dist = self.target_dist().unwrap_or(30.) * 100.;
        let dev = self.d().skills["skillsets"]["AI_skill_phoebe_bot_inventory"]["p"]["DefaultWeaponSelectionDistanceDeviation"].as_f64().unwrap_or(2000.) as f32;
        let jitter = (self.c.rand() * 2. - 1.) * dev * 0.25;
        let pick = self.c.weapons.iter().enumerate().filter(|(_, w)| w.class != WeaponClass::Melee).map(|(i, w)| {
            let ideal = match w.class { WeaponClass::Shotgun => 600., WeaponClass::Smg => 1200., WeaponClass::Pistol => 1500., WeaponClass::Rifle | WeaponClass::Minigun => 3500., WeaponClass::Sniper => 9000., WeaponClass::RocketLauncher | WeaponClass::GrenadeLauncher => 2500., WeaponClass::Melee => 100. };
            (i, (dist + jitter - ideal).abs() + if w.ammo == 0 && w.clip > 0 { 500. } else { 0. })
        }).min_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0);
        let melee = self.c.weapons.iter().position(|w| w.class == WeaponClass::Melee);
        let want = pick.or(melee);
        self.c.bb.set_obj("AIEvaluator_Global_DesiredWeapon", want.map(|i| i as u64));
    }

    fn ev_range_attack(&mut self, props: Option<&serde_json::Value>) {
        let hyst = props.and_then(|p| p.get("RangeReachHysteresisRatio")).and_then(|v| v.as_f64()).unwrap_or(1.) as f32;
        let gun = self.c.equipped.and_then(|i| self.c.weapons.get(i)).filter(|w| w.class != WeaponClass::Melee).cloned();
        let tgt = self.c.target.and_then(|t| self.agent(t).cloned());
        let (Some(w), Some(t)) = (gun, tgt) else {
            self.status("AIEvaluator_RangeAttack_ExecutionStatus", DENIED);
            self.c.bb.set_bool("AIEvaluator_RangeAttack_IsReadyToFire", false);
            return;
        };
        let d = t.feet.distance(self.c.feet);
        let range = w.range_m.max(5.);
        let los = self.c.seen.get(&t.id).map_or(false, |s| s.visible);
        let far = d > range * hyst || !los;
        self.c.bb.set_bool("AIEvaluator_Attack_MoveToDestination", far);
        self.c.bb.set_vec("AIEvaluator_Attack_Destination", Some(if far { t.feet } else { self.c.feet }));
        self.c.bb.set_float("AIEvaluator_Attack_DestinationAcceptableRadius", range * 100. * 0.8);
        let ready = los && d <= range * hyst && self.c.now - self.c.target_since >= self.c.skill.tracking_reaction;
        self.c.bb.set_bool("AIEvaluator_RangeAttack_IsReadyToFire", ready);
        self.status("AIEvaluator_RangeAttack_ExecutionStatus", PENDING);
    }

    fn ev_melee(&mut self) {
        let melee_only = !self.c.has_gun();
        let tgt = self.c.target.and_then(|t| self.agent(t).cloned());
        match (melee_only, tgt) {
            (true, Some(t)) => {
                self.c.bb.set_obj("AIEvaluator_MeleeAttack_Destination", Some(t.id));
                self.c.bb.set_bool("AIEvaluator_MeleeAttack_MoveToDestination", t.feet.distance(self.c.feet) > 1.5);
                self.status("AIEvaluator_MeleeAttack_ExecutionStatus", PENDING);
            }
            _ => { self.status("AIEvaluator_MeleeAttack_ExecutionStatus", DENIED); self.c.bb.set_bool("AIEvaluator_MeleeAttack_MoveToDestination", false); }
        }
    }

    fn ev_focus(&mut self, props: Option<&serde_json::Value>) {
        let mode = props.and_then(|p| p.get("FocusingBehavior")).and_then(|v| v.as_str()).and_then(|s| s.rsplit("::").next()).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
        let toward = props.and_then(|p| p.get("bFocusOnTargetLocation")).and_then(|v| v.as_bool()).unwrap_or(false);
        let key = props.and_then(|p| p.get("TargetLocationName")).and_then(|v| v.as_str());
        let tgt = self.c.target.and_then(|t| self.agent(t).cloned());
        let visible = tgt.as_ref().map_or(false, |t| self.c.seen.get(&t.id).map_or(false, |s| s.visible));
        let focal = if toward { key.and_then(|k| self.c.bb.vector(k)).map(|p| p + Vec3::Y * 1.2) } else {
            match mode {
                2 | 3 => None,
                4 | 6 => self.c.bb.vector("AIEvaluator_Global_LastKnownPosition").map(|p| p + Vec3::Y * 1.2),
                _ => if visible { tgt.map(|t| t.feet + Vec3::Y * 1.1) } else { None },
            }
        };
        self.c.bb.set_vec("AIEvaluator_Global_FocalPoint", focal);
        self.c.bb.set_obj("AIEvaluator_Global_FocusActor", if focal.is_some() { self.c.target } else { None });
    }

    fn ev_ads(&mut self, props: Option<&serde_json::Value>) {
        let skip = props.and_then(|p| p.get("bSkipTargetChecks")).and_then(|v| v.as_bool()).unwrap_or(false);
        let w = self.c.equipped.and_then(|i| self.c.weapons.get(i)).map(|w| w.class);
        let d = self.target_dist().unwrap_or(0.);
        self.c.intent.ads = matches!(w, Some(WeaponClass::Rifle | WeaponClass::Sniper | WeaponClass::Pistol)) && (skip || d > 12.);
        self.status("AIEvaluator_WeaponTargeting_ExecutionStatus", if self.c.intent.ads { ALLOWED } else { DENIED });
    }

    fn ev_investigate(&mut self) {
        let alert = self.c.bb.int("AIEvaluator_Global_AlertLevel");
        let lkp = self.c.bb.vector("AIEvaluator_Global_LastKnownPosition");
        if (alert == ALERT_LOST || alert == ALERT_SUSPICIOUS) && lkp.is_some() && self.c.body == Body::Ground {
            let lkp = lkp.unwrap();
            if self.c.bb.vector("AIEvaluator_Investigate_Destination").map_or(true, |d| d.distance(lkp) > 5.) {
                let (lo, hi) = self.c.skill.lkp_offset_cm;
                let r = (lo + (hi - lo) * self.c.rand()) * 0.01 * self.c.rand();
                let a = self.c.rand() * std::f32::consts::TAU;
                self.c.bb.set_vec("AIEvaluator_Investigate_Destination", Some(lkp + Vec3::new(a.cos() * r, 0., a.sin() * r)));
            }
            let reached = self.c.bb.int("AIEvaluator_Investigate_MovementState") == MOVE_SUCCESS || self.c.bb.int("AIEvaluator_Investigate_MovementState") == MOVE_BLOCKED;
            if reached {
                self.c.bb.set_int("AIEvaluator_Global_AlertLevel", ALERT_UNAWARE);
                self.c.loudness = 0.;
                self.c.bb.set_vec("AIEvaluator_Investigate_Destination", None);
                self.c.bb.set_int("AIEvaluator_Investigate_MovementState", MOVE_NONE);
                self.status("AIEvaluator_Investigate_ExecutionStatus", DENIED);
                self.c.bb.set_bool("AIEvaluator_Investigate_MoveToDestination", false);
                return;
            }
            self.c.bb.set_bool("AIEvaluator_Investigate_MoveToDestination", true);
            self.c.bb.set_bool("AIEvaluator_Investigate_Cautious", alert == ALERT_SUSPICIOUS);
            self.status("AIEvaluator_Investigate_ExecutionStatus", ALLOWED);
        } else {
            self.status("AIEvaluator_Investigate_ExecutionStatus", DENIED);
            self.c.bb.set_bool("AIEvaluator_Investigate_MoveToDestination", false);
        }
    }

    fn ev_evasive(&mut self, props: Option<&serde_json::Value>) {
        let flag = |k: &str| props.and_then(|p| p.get(k)).and_then(|v| v.as_bool()).unwrap_or(true);
        let fighting = self.c.target.is_some() && self.c.body == Body::Ground;
        self.status("AIEvaluator_Dodge_ExecutionStatus", DENIED);
        if !fighting { self.status("AIEvaluator_Crouch_ExecutionStatus", DENIED); self.status("AIEvaluator_Jump_ExecutionStatus", DENIED); return; }
        if self.c.now < self.c.evasive_next { return; }
        let delay = self.c.dev(self.c.skill.wobble_delay, self.c.skill.wobble_dev);
        self.c.evasive_next = self.c.now + delay.max(0.5);
        let u = self.c.rand();
        if flag("bDoJumping") && u < 0.35 { self.status("AIEvaluator_Jump_ExecutionStatus", ALLOWED); }
        else if flag("bDoCrouching") && u < 0.7 { let cur = self.c.bb.int("AIEvaluator_Crouch_ExecutionStatus"); self.status("AIEvaluator_Crouch_ExecutionStatus", if cur == ALLOWED { DENIED } else { ALLOWED }); }
    }

    fn manage_weapon(&mut self) {
        let want = self.c.bb.object("AIEvaluator_Global_DesiredWeapon").map(|i| i as usize).filter(|i| *i < self.c.weapons.len());
        if want.is_some() && want != self.c.equipped { self.c.equipped = want; self.c.next_shot = self.c.now + 0.4; self.c.hit_timing = None; }
        let now = self.c.now;
        let Some(wi) = self.c.equipped else { return };
        let w = self.c.weapons[wi].clone();
        if w.class == WeaponClass::Melee { return; }
        if w.clip > 0 && w.ammo == 0 && now >= self.c.reload_until {
            self.c.reload_until = now + w.reload_time.max(0.5);
            self.c.weapons[wi].ammo = w.clip;
            self.c.bb.set_bool("AIEvaluator_ManageWeapon_Reload", true);
            return;
        }
        self.c.bb.set_bool("AIEvaluator_ManageWeapon_Reload", false);
        let ready = self.c.bb.boolean("AIEvaluator_RangeAttack_IsReadyToFire");
        self.c.bb.set_bool("AIEvaluator_ManageWeapon_Fire", ready);
    }

    fn fire_step(&mut self, dt: f32) {
        let now = self.c.now;
        let (Some(wi), Some(tid)) = (self.c.equipped, self.c.target) else { return };
        let Some(t) = self.agent(tid).cloned() else { return };
        let w = self.c.weapons[wi].clone();
        if w.class == WeaponClass::Melee {
            if now >= self.c.next_shot && self.c.bb.int("AIEvaluator_MeleeAttack_ExecutionStatus") >= PENDING && t.feet.distance(self.c.feet) < 2.2 {
                let dir = (t.feet + Vec3::Y * 1.0 - self.c.eye()).normalize_or_zero();
                self.c.intent.fire = Some(FireRequest { weapon: wi, dir, hit: true, target: tid });
                self.c.next_shot = now + w.fire_interval.max(0.4);
                self.c.stats.shots += 1;
            }
            return;
        }
        if !self.c.bb.boolean("AIEvaluator_ManageWeapon_Fire") || now < self.c.reload_until || now < self.c.next_shot { return; }
        let dist_cm = t.feet.distance(self.c.feet) * 100.;
        let pattern = crate::skill::firing_pattern(self.d(), w.class, dist_cm);
        self.c.pattern_t += dt;
        let hold = self.d().row(pattern, "", self.c.pattern_t).unwrap_or(1.) > 0.5;
        if !hold { return; }
        let level = self.c.skill.level;
        if self.c.hit_timing.map_or(true, |(c, _)| c != w.class) {
            let ht = HitTiming::new(self.d(), w.class, level);
            self.c.hit_timing = Some((w.class, ht));
            let u = self.c.rand();
            self.c.next_hit = now + ht.between(dist_cm, u) * 0.5;
        }
        let (_, ht) = self.c.hit_timing.unwrap();
        let hit = !ht.force_miss || now >= self.c.next_hit;
        if hit { let u = self.c.rand(); self.c.next_hit = now + ht.between(dist_cm, u); self.c.stats.hits_planned += 1; }
        let eye = self.c.eye();
        let aim_at = t.feet + Vec3::Y * if ht.prevent_headshots { 1.05 } else { 1.5 };
        let base = (aim_at - eye).normalize_or_zero();
        let (min_a, max_a) = crate::skill::circle_angles(self.d(), dist_cm);
        let r = 0.42_f32;
        let miss_min = (r / (dist_cm * 0.01).max(0.5)).atan().to_degrees() * 1.6;
        let ang = if hit { min_a * self.c.rand() * 0.5 } else { miss_min.max(min_a) + (max_a - min_a).max(0.) * self.c.rand() };
        let side = base.cross(Vec3::Y).normalize_or_zero();
        let up = side.cross(base).normalize_or_zero();
        let phi = self.c.rand() * std::f32::consts::TAU;
        let off = (side * phi.cos() + up * phi.sin() * 0.6) * ang.to_radians().tan();
        let dir = (base + off).normalize_or_zero();
        self.c.intent.fire = Some(FireRequest { weapon: wi, dir, hit, target: tid });
        self.c.next_shot = now + w.fire_interval.max(0.05);
        if w.clip > 0 { self.c.weapons[wi].ammo = w.ammo.saturating_sub(1); }
        self.c.stats.shots += 1;
    }
}

pub fn denied_keys(class: &str) -> &'static [&'static str] {
    match class {
        "FortAthenaAIBotEvaluator_CharacterLaunched" => &["AIEvaluator_CharacterLaunched_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_DBNO" => &["AIEvaluator_DBNO_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_Revive" => &["AIEvaluator_Revive_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_AvoidThreat" => &["AIEvaluator_AvoidThreat_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_StealWall" => &["AIEvaluator_StealWallBuilding_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_Escape" => &["AIEvaluator_Escape_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_DangerDetection" => &["AIEvaluator_DangerZoneDetected_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_TrapOnPathDetected" => &["AIEvaluator_TrapOnPath_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_SandTunnel" => &[],
        "FortAthenaAIBotEvaluator_Ground" => &[],
        "FortAthenaAIBotEvaluator_Retreat" => &["AIEvaluator_Retreat_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_ThrowableAttack" => &["AIEvaluator_ThrowableAttack_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_Ambush" => &["AIEvaluator_Ambush_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_Bunker" => &["AIEvaluator_Bunker_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_TakeCover" => &["AIEvaluator_TakeCover_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_Observe" => &["AIEvaluator_Observe_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_DefensiveBuilding" => &["AIEvaluator_DefensiveBuilding_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_DanceOnKill" => &[],
        "FortAthenaAIBotEvaluator_Heal" => &["AIEvaluator_Healing_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_ReloadWeapon" => &["AIEvaluator_WeaponReload_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_SmartObjects" => &["AIEvaluator_SmartObject_ExecutionStatus"],
        "FortAthenaAIBotEvaluator_Zipline" => &["AIEvaluator_Zipline_MoveExecutionStatus", "AIEvaluator_Zipline_UsageExecutionStatus"],
        _ => &[],
    }
}

pub const INJECTION_KEYS: [&str; 3] = ["AIEvaluator_Injection_Threatened_HighPriority", "AIEvaluator_Injection_Unaware_HighPriority", "AIEvaluator_Injection_Alive_HighPriority"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin { Port, Rebuilt, RebuiltDenied, Engine }

pub fn node_origin(class: &str) -> (Origin, &'static str) {
    match class {
        "BTComposite_Selector" | "BTComposite_Sequence" | "BTComposite_SimpleParallel" | "BTTask_RunBehavior" | "BTTask_Wait" | "BTDecorator_Blackboard" | "BTDecorator_Loop" | "BTDecorator_ForceSuccess" | "BTTask_MoveDirectlyToward" => (Origin::Engine, "UE 5.3 AIModule semantics"),
        "FortAthenaBTService_Crouch" => (Origin::Port, "OnBlackboardKeyValueChange 0xaa22274, OnCheckCrouchResult 0xaa23450"),
        "FortAthenaBTService_CopyBlackboardVariable" => (Origin::Port, "CopyBlackboardValues 0xaa12c10, OnBecomeRelevant 0xaa1ddd8"),
        "FortAthenaAIBotEvaluator_SelectNextDynamicPOI" => (Origin::Port, "SelectNextPOI 0xaa08844 / SetDestination 0xaa095f8 control flow; the POI choice itself is REBUILT from Phoebe_Data BotPOIScoring* rows"),
        "FortAthenaAIBotEvaluator_JumpOffBus" | "FortAthenaAIBotEvaluator_FreeFalling" | "FortAthenaAIBotEvaluator_Warmup" | "FortAthenaAIBotEvaluator_Loot" | "FortAthenaAIBotEvaluator_Sprinting" | "FortAthenaAIBotEvaluator_SelectNextPOI"
        | "FortAthenaAIBotEvaluator_Storm" | "FortAthenaAIBotEvaluator_PatrolAround" | "FortAthenaAIBotEvaluator_WeaponSelection" | "FortAthenaAIBotEvaluator_RangeAttack" | "FortAthenaAIBotEvaluator_MeleeAttack"
        | "FortAthenaAIBotEvaluator_HandleFocusing" | "FortAthenaAIBotEvaluator_AimDownSight" | "FortAthenaAIBotEvaluator_Investigate" | "FortAthenaAIBotEvaluator_EvasiveManeuvers"
        | "FortAthenaBTService_UpdateTarget" | "FortAthenaBTService_ManageWeapon" | "FortAthenaBTService_Jump" | "FortAthenaBTService_Sprinting" | "FortAthenaBTService_JumpOffBus"
        | "FortAthenaBTService_SetBlackboardBool" | "FortAthenaBTTask_BotMoveTo" | "FortAthenaBTTask_Dive" | "FortAthenaBTTask_Glide" | "FortAthenaBTTask_Interact" | "FortAthenaBTTask_PlayEmote" | "FortAthenaAIBotEvaluator_PlayEmote" | "FortBTDecorator_DistanceBetween" | "FortAthenaBTDecorator_BehaviorControls" | "FortAthenaBTService_AIEvaluator" => (Origin::Rebuilt, "server-only (FortniteAIServer) or stripped; rebuilt from node data and BB keys"),
        _ => (Origin::RebuiltDenied, "server-only, needs systems the 1.8 port lacks; writes ExecutionDenied / fails so its branch is skipped"),
    }
}

impl<'a> Host for Ctx<'a> {
    fn bb(&self) -> &Blackboard { &self.c.bb }
    fn now(&self) -> f32 { self.c.now }
    fn rand01(&mut self) -> f32 { self.c.rand() }
    fn decorator(&mut self, g: &Graph, aux: u32) -> bool {
        let a = &g.aux[aux as usize];
        match a.class.as_str() {
            "FortBTDecorator_DistanceBetween" => {
                let from = a.key("BlackboardKeyA").and_then(|k| self.key_location(g, k)).unwrap_or(self.c.feet);
                let Some(to) = a.key("BlackboardKeyB").and_then(|k| self.key_location(g, k)) else { return false };
                let d = if a.p.get("bCalculateAs2D").and_then(|v| v.as_bool()).unwrap_or(false) { Vec2::new(to.x - from.x, to.z - from.z).length() } else { to.distance(from) } * 100.;
                crate::bb::test_arith(d as f64, a.p.get("Operator").and_then(|v| v.as_i64()).unwrap_or(0) as i32, a.f("SpecifiedDistance") as f64)
            }
            "FortAthenaBTDecorator_BehaviorControls" => true,
            _ => false,
        }
    }
    fn service_activate(&mut self, g: &Graph, aux: u32) {
        let a = &g.aux[aux as usize];
        match a.class.as_str() {
            "FortAthenaBTService_CopyBlackboardVariable" => { if a.p.get("bCopyOnBecomeRelevant").and_then(|v| v.as_bool()).unwrap_or(true) { self.copy_bb(g, aux); } }
            "FortAthenaBTService_SetBlackboardBool" => { if let Some(k) = a.key("BlackboardKey") { self.c.bb.set(k, Val::Bool(false)); } }
            "FortBTService_AIEvaluatorsInjector" => { for k in INJECTION_KEYS { self.status(k, DENIED); } }
            "FortAthenaBTService_AIEvaluator" => { self.service_tick(g, aux, 0.); }
            _ => {}
        }
    }
    fn service_deactivate(&mut self, g: &Graph, aux: u32) {
        let a = &g.aux[aux as usize];
        match a.class.as_str() {
            "FortAthenaBTService_CopyBlackboardVariable" => { if a.p.get("bCopyOnCeaseRelevant").and_then(|v| v.as_bool()).unwrap_or(false) { self.copy_bb(g, aux); } }
            "FortAthenaBTService_SetBlackboardBool" => { if let Some(k) = a.key("BlackboardKey") { self.c.bb.set(k, Val::Bool(true)); } }
            "FortAthenaBTService_Sprinting" => { self.c.intent.sprint = false; }
            _ => {}
        }
    }
    fn service_tick(&mut self, g: &Graph, aux: u32, _dt: f32) {
        let a = &g.aux[aux as usize];
        match a.class.as_str() {
            "FortAthenaBTService_AIEvaluator" => {
                let Some(cls) = a.s("AIEvaluatorClass") else { return };
                let data = self.c.data.clone();
                let (native, props) = data.evaluator_class(cls);
                self.evaluator(&native, props);
            }
            "FortAthenaBTService_ManageWeapon" => self.manage_weapon(),
            "FortAthenaBTService_Crouch" => {
                let s = self.c.bb.int("AIEvaluator_Crouch_ExecutionStatus");
                if s == ALLOWED { self.c.crouch_req = Some(true); } else if s == DENIED { self.c.crouch_req = Some(false); }
            }
            "FortAthenaBTService_Jump" => {
                if self.c.bb.int("AIEvaluator_Jump_ExecutionStatus") == ALLOWED { self.c.jump_pulse = true; self.status("AIEvaluator_Jump_ExecutionStatus", SUCCESS); }
            }
            "FortAthenaBTService_Sprinting" => {
                self.c.intent.sprint = self.c.bb.int("AIEvaluator_Sprinting_ExecutionStatus") == ALLOWED && self.c.bb.boolean("AIEvaluator_Sprinting_AllowSprint");
            }
            "FortAthenaBTService_JumpOffBus" => {
                if self.c.body == Body::Bus && self.c.bb.int("AIEvaluator_JumpOffBus_ExecutionStatus") == ALLOWED { self.c.intent.exit_bus = true; }
            }
            "FortAthenaBTService_UpdateTarget" => {}
            "FortAthenaBTService_CopyBlackboardVariable" => self.copy_bb(g, aux),
            _ => {}
        }
    }
    fn task_start(&mut self, g: &Graph, node: u32) -> Status {
        let n = &g.nodes[node as usize];
        match n.class.as_str() {
            "BTTask_Wait" => {
                let t = self.c.dev(n.f("WaitTime"), n.f("RandomDeviation"));
                self.c.wait_until.insert(node, self.c.now + t);
                Status::InProgress
            }
            "FortAthenaBTTask_BotMoveTo" | "BTTask_MoveDirectlyToward" => {
                let Some(k) = n.key("BlackboardKey") else { return Status::Failed };
                let Some(goal) = self.key_location(g, k) else { return Status::Failed };
                let radius = n.f("AcceptableRadius") * 0.01;
                self.set_goal(goal, radius, n.class == "BTTask_MoveDirectlyToward");
                if let Some(rk) = n.key("MovementResultKeyName") { self.c.bb.set(rk, Val::Int(MOVE_IN_PROGRESS)); }
                Status::InProgress
            }
            "FortAthenaBTTask_Interact" => {
                let Some(id) = n.key("InteractContextInfoKeySelector").and_then(|k| match self.c.bb.get(k) { Val::Obj(o) => o, _ => None }) else { return Status::Failed };
                let t = { let sk = self.c.skill.clone(); self.c.dev(sk.loot_pickup_time, sk.loot_pickup_dev) };
                self.c.interacting = Some((id, self.c.now + t));
                self.stop_move();
                Status::InProgress
            }
            "FortAthenaBTTask_Dive" | "FortAthenaBTTask_Glide" => Status::InProgress,
            "FortAthenaBTTask_PlayEmote" => {
                if self.c.bb.int("AIEvaluator_PlayEmote_ExecutionStatus") < PENDING { return Status::Failed; }
                let (lo, hi) = (4., 9.);
                let t = lo + (hi - lo) * self.c.rand();
                self.c.emote_until = self.c.now + t;
                self.c.intent.emote = Some((self.c.rand() * 1000.) as u32);
                self.stop_move();
                Status::InProgress
            }
            _ => Status::Failed,
        }
    }
    fn task_tick(&mut self, g: &Graph, node: u32, dt: f32) -> Status {
        let n = &g.nodes[node as usize];
        match n.class.as_str() {
            "BTTask_Wait" => {
                self.stop_move_soft();
                self.fire_step(dt);
                if self.c.now >= self.c.wait_until.get(&node).copied().unwrap_or(0.) { Status::Succeeded } else { Status::InProgress }
            }
            "FortAthenaBTTask_BotMoveTo" | "BTTask_MoveDirectlyToward" => {
                if n.b("bObserveBlackboardValue") || n.class == "FortAthenaBTTask_BotMoveTo" {
                    if let Some(goal) = n.key("BlackboardKey").and_then(|k| self.key_location(g, k)) {
                        let tol = n.f("ObservedBlackboardValueTolerance").max(50.) * 0.01;
                        if self.c.nav.goal.map_or(true, |cur| cur.distance(goal) > tol) { self.c.nav.goal = None; self.set_goal(goal, n.f("AcceptableRadius") * 0.01, n.class == "BTTask_MoveDirectlyToward"); }
                    } else { self.stop_move(); return Status::Failed; }
                }
                let s = self.move_status();
                let dir = self.steer();
                self.c.intent.move_dir = dir;
                self.fire_step(dt);
                let rk = n.key("MovementResultKeyName");
                match s {
                    MOVE_SUCCESS => { if let Some(rk) = rk { self.c.bb.set(rk, Val::Int(MOVE_SUCCESS)); } self.stop_move(); Status::Succeeded }
                    MOVE_BLOCKED | MOVE_NO_PATH => { if let Some(rk) = rk { self.c.bb.set(rk, Val::Int(s)); } self.stop_move(); Status::Failed }
                    _ => Status::InProgress,
                }
            }
            "FortAthenaBTTask_Interact" => {
                let Some((id, until)) = self.c.interacting else { return Status::Failed };
                if self.c.now < until { return Status::InProgress; }
                self.c.intent.interact = Some(id);
                if self.c.interact_done == Some(id) || self.c.looted.contains(&id) {
                    if let Some(k) = n.key("InteractExecutionStatusKeySelector") { self.c.bb.set(k, Val::Int(SUCCESS)); }
                    self.c.intent.interact = None;
                    self.c.stats.loot_interactions += 1;
                    if let Some(d) = self.c.interact_done.take() { if !self.c.looted.contains(&d) { self.c.looted.push(d); } }
                    self.c.interacting = None;
                    return Status::Succeeded;
                }
                if self.c.now > until + 6. { self.c.looted.push(id); self.c.intent.interact = None; self.c.interacting = None; return Status::Failed; }
                Status::InProgress
            }
            "FortAthenaBTTask_PlayEmote" => {
                if self.c.now < self.c.emote_until { return Status::InProgress; }
                self.c.intent.emote = None;
                self.c.emote_cooldown = self.c.now + 1. + 2. * self.c.rand();
                self.c.bb.set_vec("AIEvaluator_PlayEmote_Destination", None);
                self.status("AIEvaluator_PlayEmote_ExecutionStatus", DENIED);
                Status::Succeeded
            }
            "FortAthenaBTTask_Dive" => {
                if self.c.body != Body::Skydive { return Status::Succeeded; }
                self.aerial_steer(self.c.bb.vector("AIEvaluator_Dive_Destination"), true);
                Status::InProgress
            }
            "FortAthenaBTTask_Glide" => {
                if self.c.body != Body::Parachute { self.c.intent.aerial = Vec2::ZERO; return Status::Succeeded; }
                self.aerial_steer(self.c.bb.vector("AIEvaluator_Glide_Destination"), false);
                Status::InProgress
            }
            _ => Status::Failed,
        }
    }
    fn task_abort(&mut self, g: &Graph, node: u32) {
        let n = &g.nodes[node as usize];
        match n.class.as_str() {
            "FortAthenaBTTask_BotMoveTo" | "BTTask_MoveDirectlyToward" => { if let Some(rk) = n.key("MovementResultKeyName") { self.c.bb.set(rk, Val::Int(MOVE_ABORTED)); } self.stop_move(); }
            "FortAthenaBTTask_Interact" => { self.c.interacting = None; self.c.intent.interact = None; }
            "FortAthenaBTTask_PlayEmote" => { self.c.intent.emote = None; self.c.bb.set_vec("AIEvaluator_PlayEmote_Destination", None); }
            "FortAthenaBTTask_Dive" | "FortAthenaBTTask_Glide" => { self.c.intent.aerial = Vec2::ZERO; }
            _ => {}
        }
    }
}

impl<'a> Ctx<'a> {
    fn stop_move_soft(&mut self) { self.c.intent.move_dir = Vec2::ZERO; }
    fn copy_bb(&mut self, g: &Graph, aux: u32) {
        let a = &g.aux[aux as usize];
        if let (Some(s), Some(d)) = (a.key("SourceBlackboardKey"), a.key("DestinationBlackboardKey")) { let v = self.c.bb.get(s); self.c.bb.set(d, v); }
    }
    fn aerial_steer(&mut self, dest: Option<Vec3>, dive: bool) {
        let Some(dest) = dest else { self.c.intent.aerial = Vec2::ZERO; return };
        let d = Vec2::new(dest.x - self.c.feet.x, dest.z - self.c.feet.z);
        let h = (self.c.feet.y - dest.y).max(1.);
        let close = d.length() < h * if dive { 0.35 } else { 0.15 } || d.length() < self.c.skill.glider_landing_cm * 0.01;
        self.c.intent.aerial = if close { Vec2::ZERO } else { d.normalize_or_zero() };
        self.c.intent.aerial_dive = dive && close;
        self.c.yaw = if d.length_squared() > 1. { yaw_to(Vec3::new(d.x, 0., d.y)) } else { self.c.yaw };
        self.c.pitch = if dive && close { -80. } else { -30. };
    }
}
