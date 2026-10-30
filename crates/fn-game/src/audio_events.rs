//! Renderer-free, audio-engine-free detection of the game moments that make sound (the front-end maps them to its sound events).
//!
//! Everything is read from the real game state after a frame / tick; there are no timers:
//!
//! | moment | source |
//! |---|---|
//! | [`AudioMoment::Step`] | the `AnimNotify_FootStep_Left/Right` (`LeftFootstep` / `RightFootstep`) notifies of the locomotion clip the animation graph is playing, fired when the clip phase crosses them (a [`StepTable`] read from the clips; the leader is the highest-weighted clip of the highest-weighted clip family, i.e. the blend space `NotifyTriggerMode = HighestWeightedAnimation`); gait from the clip family |
//! | [`AudioMoment::Jump`] | `Controller::airborne()` rising edge with an upward velocity (liftoff) |
//! | [`AudioMoment::Land`] | `airborne()` falling edge; impact speed = `sqrt(vz^2 + 2 g h)` of the last airborne observation |
//! | [`AudioMoment::Swing`] | `World::strike` ran = the pickaxe montage's `TriggerAbility` notify (0.1603 s / 0.2015 s into Swing1 / Swing2) = the weapon fires (`PrimaryFireSound`) |
//! | [`AudioMoment::Harvest`] | a prop's health dropped (kind -> surface; `destroyed` on the killing hit) |
//! | [`AudioMoment::Equip`] | the `Melee_Equip` montage started |
//!
//! Crouch / un-crouch have no cue in the 1.8 data (`fn_audio::events::TABLE`), so they produce no moment.
use crate::sim::{GameSim, EYE};
use fn_animgraph::Node;
use fn_combat::harvest_fx::FxEvent;
use fn_combat::{Kind, CHEST_HEIGHT, HARVEST_DAMAGE};
use glam::Vec3;
use std::collections::HashMap;

/// Footstep cue variant (`PlayerPawn_Athena_C` `Default{,Fast,Crouched,CrouchSprint}FootstepSound`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gait {
    Walk,
    Sprint,
    Crouch,
    CrouchSprint,
}

/// What a harvest hit struck (by the prop's kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitSurface {
    Tree,
    Rock,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AudioMoment {
    Step { gait: Gait, left: bool },
    Jump,
    /// `speed` = impact speed in m/s.
    Land { speed: f32 },
    /// Pickaxe weapon fire at the pawn's chest (world metres).
    Swing { pos: Vec3 },
    /// A prop was hit; `pos` is the point on its surface facing the pawn, `destroyed` on the killing hit.
    /// `proto` = `Harvestable::proto` (`fn_combat::NO_PROTO` for a placeholder): the front-end maps it to the prop's own hit / death sounds.
    Harvest { surface: HitSurface, destroyed: bool, pos: Vec3, proto: u32 },
    /// A prop died with no hit this step (BUILDING: a placement destroyed it, `bDestroyOnPlayerBuildingPlacement`): its death sound only.
    PropDestroyed { surface: HitSurface, pos: Vec3, proto: u32 },
    Equip,
    // CRACK AUDIO HOOK: the weak-spot effect, from `world.fx` (`FxEvent`s), at the weak spot (world metres).
    /// `FxEvent::WeakSpotSpawned`: the hit that generated a weak spot (`Crack_Spawn_Cue`, `CrackHitCount` 0).
    CrackSpawn { pos: Vec3 },
    /// `FxEvent::WeakSpotHit`: `Sound_Crack_Level_0N_Cue` (`level` 0..=8) ...
    CrackLevel { level: u8, pos: Vec3 },
    /// ... and `Crack_Impact_Cue`.
    CrackImpact { pos: Vec3 },
}

/// The footstep notifies of one locomotion clip.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipSteps {
    /// Sequence length in seconds.
    pub length: f32,
    /// (notify time in seconds, is the left foot).
    pub steps: Vec<(f32, bool)>,
    /// Clips of one blend space / state share a family (the direction token is dropped from the name).
    pub family: u16,
    pub gait: Gait,
}

/// Clip name -> footstep notifies (only clips that have some).
#[derive(Clone, Debug, Default)]
pub struct StepTable {
    pub clips: HashMap<String, ClipSteps>,
    families: Vec<String>,
}

const DIRECTIONS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
/// `M_Jog_AssaultRifle_NE_Core` -> `M_Jog_AssaultRifle_Core` (all direction samples of a blend space are one family).
pub fn family_key(name: &str) -> String {
    name.split('_').filter(|t| !DIRECTIONS.contains(t)).collect::<Vec<_>>().join("_")
}
/// Gait of a locomotion clip: `CrouchSprint_*` / `Sprint_*` / `Crouch_*` / everything else (walk, jog, starts).
pub fn clip_gait(name: &str) -> Gait {
    let l = name.to_ascii_lowercase();
    if l.contains("crouchsprint") || (l.contains("crouch") && l.contains("sprint")) { Gait::CrouchSprint } else if l.starts_with("sprint") || l.contains("_sprint") { Gait::Sprint } else if l.contains("crouch") { Gait::Crouch } else { Gait::Walk }
}

impl StepTable {
    /// Builds the table from `(clip name, length, [(notify time, left foot)])`; clips without footstep notifies are skipped.
    pub fn from_clips(clips: impl IntoIterator<Item = (String, f32, Vec<(f32, bool)>)>) -> StepTable {
        let mut t = StepTable::default();
        for (name, length, steps) in clips {
            if steps.is_empty() || length <= 0. { continue; }
            let key = family_key(&name);
            let family = match t.families.iter().position(|f| *f == key) { Some(i) => i, None => { t.families.push(key); t.families.len() - 1 } } as u16;
            let gait = clip_gait(&name);
            t.clips.insert(name, ClipSteps { length, steps, family, gait });
        }
        t
    }
    pub fn is_empty(&self) -> bool { self.clips.is_empty() }
    pub fn families(&self) -> &[String] { &self.families }
}

/// Weighted clip leaves of a pose tree (`Mix` is normalised; additive layers, aim offsets and IK add no footsteps; a layered lower-body blend
/// pose owns the legs). `f(name, time, looping, weight)`.
pub fn collect_clips(n: &Node, w: f32, f: &mut dyn FnMut(&str, f32, bool, f32)) {
    match n {
        Node::Clip { name, time, looping } => f(name, *time, *looping, w),
        Node::Mix(v) => {
            let total: f32 = v.iter().map(|(_, x)| *x).sum();
            if total > 1e-6 { for (c, x) in v { collect_clips(c, w * x / total, f); } }
        }
        Node::MixProfile { from, to, w: t, .. } => { collect_clips(from, w * (1. - t), f); collect_clips(to, w * t, f); }
        Node::ApplyAdditive { base, .. } | Node::ApplyMeshAdditive { base, .. } => collect_clips(base, w, f),
        Node::Layered { base, blend, weight, .. } => { collect_clips(base, w * (1. - weight), f); collect_clips(blend, w * weight, f); }
        Node::RotateRoot { base, .. } | Node::AimOffset { base, .. } | Node::TwoBoneIk { base, .. } | Node::HandRetarget { base, .. } | Node::CopyBone { base, .. } | Node::ModifyBone { base, .. } | Node::HeadTrack { base, .. } => collect_clips(base, w, f),
        Node::Ref => {}
    }
}

/// Smallest clip family weight that may trigger footsteps (a blend leaving a state stops firing its notifies below this).
pub const LEADER_MIN_WEIGHT: f32 = 0.25;
/// Two foot plants of the same foot closer than this are one (marker-synced blend spaces switch their leader sample mid-cycle).
pub const MIN_SAME_FOOT_INTERVAL: f32 = 0.15;
/// Speed (m/s) below which the pawn is not walking (no footsteps from a clip that is only fading out).
pub const MIN_STEP_SPEED: f32 = 0.2;

struct Leader { family: u16, gait: Gait, phase: f32 }

pub struct MomentDetector {
    primed: bool,
    clock: f32,
    was_air: bool,
    last_vz: f32,
    last_h: f32,
    hits: u32,
    misses: u32,
    props: Vec<(f32, u32)>,
    equip_active: bool,
    prev_leader: Option<(u16, f32)>,
    last_foot: [f32; 2],
    /// CRACK AUDIO HOOK: `world.fx.emitted` already handled (None until the first observation).
    crack_seen: Option<u64>,
}
impl Default for MomentDetector {
    fn default() -> Self { MomentDetector { primed: false, clock: 0., was_air: false, last_vz: 0., last_h: 0., hits: 0, misses: 0, props: vec![], equip_active: false, prev_leader: None, last_foot: [-10.; 2], crack_seen: None } }
}

impl MomentDetector {
    pub fn new() -> Self { Self::default() }

    fn snapshot_props(&mut self, g: &GameSim) { self.props.clear(); self.props.extend(g.world.props.iter().map(|p| (p.health, p.life))); }

    /// Looks at the state after a frame (one `GameSim::step`, or the ticks of one rendered frame) of `dt` seconds and appends what happened.
    /// `steps` = the footstep notify table (None while it is still loading: no footsteps yet).
    pub fn observe(&mut self, g: &GameSim, steps: Option<&StepTable>, dt: f32, out: &mut Vec<AudioMoment>) {
        self.clock += dt.max(0.);
        let c = &g.controller;
        let air = c.airborne();
        // ---- jump / landing
        if air && !self.was_air && c.vertical_speed > 0.5 { out.push(AudioMoment::Jump); }
        if !air && self.was_air {
            let gr = fn_movement::gravity();
            out.push(AudioMoment::Land { speed: (self.last_vz * self.last_vz + 2. * gr * self.last_h.max(0.)).sqrt() });
        }
        self.was_air = air;
        self.last_vz = c.vertical_speed;
        self.last_h = c.air_height();
        // ---- equip montage
        if g.anim.equip.active && !self.equip_active { out.push(AudioMoment::Equip); }
        self.equip_active = g.anim.equip.active;
        // ---- melee: strike() ran = the TriggerAbility notify; its prop hits
        let pawn = Vec3::new(g.x, g.y - EYE + CHEST_HEIGHT, g.z);
        if !self.primed || g.world.props.len() != self.props.len() || g.world.hits < self.hits || g.world.misses < self.misses {
            self.hits = g.world.hits; self.misses = g.world.misses; self.snapshot_props(g);
        } else {
            let strikes = (g.world.hits - self.hits) + (g.world.misses - self.misses);
            for _ in 0..strikes { out.push(AudioMoment::Swing { pos: pawn }); }
            for (i, p) in g.world.props.iter().enumerate() {
                let (was, life) = std::mem::replace(&mut self.props[i], (p.health, p.life));
                if life == p.life && p.health < was && !p.alive() && g.world.hits == self.hits {
                    // BUILDING: no strike this step (a placement destroyed it): the death cue at the prop, no impact
                    out.push(AudioMoment::PropDestroyed { surface: match p.kind { Kind::Tree => HitSurface::Tree, Kind::Rock => HitSurface::Rock }, pos: Vec3::new(p.pos.x, p.pos.y + CHEST_HEIGHT, p.pos.z), proto: p.proto });
                } else if life == p.life && p.health < was {
                    // HARVEST HOOK: a weak-point hit takes 100 HP in ONE strike (two ordinary hits take 50 each): the number of impacts is the number of strikes that hit,
                    // never more than the damage could be made of
                    let by_damage = (((was - p.health) / HARVEST_DAMAGE).ceil() as u32).max(1);
                    let n = (g.world.hits.saturating_sub(self.hits)).clamp(1, by_damage);
                    let dir = Vec3::new(pawn.x - p.pos.x, 0., pawn.z - p.pos.z).normalize_or_zero();
                    let pos = Vec3::new(p.pos.x, p.pos.y + CHEST_HEIGHT, p.pos.z) + dir * p.radius;
                    for k in 0..n { out.push(AudioMoment::Harvest { surface: match p.kind { Kind::Tree => HitSurface::Tree, Kind::Rock => HitSurface::Rock }, destroyed: !p.alive() && k + 1 == n, pos, proto: p.proto }); }
                }
            }
            self.hits = g.world.hits; self.misses = g.world.misses;
        }
        // CRACK AUDIO HOOK: the weak-spot effect. `Fx::events` is drained by the presentation once per presented frame (after the audio observed every sim step), so the events of this
        // step are the newest `emitted - seen` entries; nothing is drained here. The first observation only takes the counter (events before the detector started are not replayed).
        let emitted = g.world.fx.emitted;
        if let Some(seen) = self.crack_seen {
            let new = emitted.saturating_sub(seen) as usize;
            let evs = &g.world.fx.events;
            for e in &evs[evs.len().saturating_sub(new)..] {
                match *e {
                    FxEvent::WeakSpotSpawned { pos, .. } => out.push(AudioMoment::CrackSpawn { pos }),
                    FxEvent::WeakSpotHit { pos, level, .. } => { out.push(AudioMoment::CrackLevel { level, pos }); out.push(AudioMoment::CrackImpact { pos }); }
                    _ => {} // Hit (ordinary) / Broke: the prop's own hit / death sounds come from the `Harvest` moment
                }
            }
        }
        self.crack_seen = Some(emitted);
        // ---- footsteps from the clip notifies
        if let Some(t) = steps { self.footsteps(g, t, out); }
        self.primed = true;
    }

    fn leader<'a>(g: &GameSim, t: &'a StepTable) -> Option<(Leader, &'a ClipSteps)> {
        let tree = g.pose_tree();
        let mut fam = [0f32; 24];
        // (weight, clip, time, looping) of the heaviest clip per family
        let mut best: [Option<(f32, &ClipSteps, f32, bool)>; 24] = [None; 24];
        collect_clips(&tree, 1., &mut |name, time, looping, w| {
            let Some(cs) = t.clips.get(name) else { return };
            let f = cs.family as usize;
            if f >= fam.len() { return; }
            fam[f] += w;
            if best[f].map_or(true, |b| w > b.0) { best[f] = Some((w, cs, time, looping)); }
        });
        let (f, tot) = fam.iter().copied().enumerate().max_by(|a, b| a.1.total_cmp(&b.1))?;
        if tot < LEADER_MIN_WEIGHT { return None; }
        let (_, cs, time, looping) = best[f]?;
        let ph = time / cs.length;
        let phase = if looping { ph.rem_euclid(1.) } else { ph.clamp(0., 1.) };
        Some((Leader { family: cs.family, gait: cs.gait, phase }, cs))
    }

    fn footsteps(&mut self, g: &GameSim, t: &StepTable, out: &mut Vec<AudioMoment>) {
        let c = &g.controller;
        if c.airborne() || c.velocity.length() < MIN_STEP_SPEED { self.prev_leader = None; return; }
        let Some((l, cs)) = Self::leader(g, t) else { self.prev_leader = None; return };
        if let Some((fam, prev)) = self.prev_leader {
            if fam == l.family {
                let cur = l.phase;
                for &(nt, left) in &cs.steps {
                    let p = nt / cs.length;
                    let crossed = if prev <= cur { p > prev && p <= cur } else { p > prev || p <= cur };
                    let slot = left as usize;
                    if crossed && self.clock - self.last_foot[slot] >= MIN_SAME_FOOT_INTERVAL {
                        self.last_foot[slot] = self.clock;
                        out.push(AudioMoment::Step { gait: l.gait, left });
                    }
                }
            }
        }
        self.prev_leader = Some((l.family, l.phase));
    }
}

// ---- WEAPON AUDIO HOOK: bullet pass-by ("whip") ------------------------------------------------------------------------------------------------
/// The pass-by radii (UE centimetres) of the two Blueprints that play the bullet whip: the tracer actors (`TracerGeneric_Athena` / `TracerGeneric`
/// `PassByRadiusMin` 100 / `PassByRadiusMax` 400) and `BulletWhipTrackerComponent` (50 / 400).
pub const TRACER_PASS_BY_RADII: (f32, f32) = (100., 400.);
pub const WHIP_COMPONENT_RADII: (f32, f32) = (50., 400.);

/// What `PlayPassBySound` plays: `whip.far` (`PassByFarSound`) with `far_volume` and `whip.close` (`PassByCloseSound`) with `close_volume`, both at `location`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhipSound { pub location: Vec3, pub far_volume: f32, pub close_volume: f32 }

/// Port of the tracer Blueprint's `TrackPassBy` + `PlayPassBySound` (bytecode of `TracerGeneric_Athena_C`, identical in `BulletWhipTrackerComponent_C`).
/// ALL VECTORS ARE UE-SPACE (centimetres, Z up) exactly as the Blueprint computes them; the Blueprint only whips for bullets whose instigator is NOT the
/// local pawn (`GetLocalPawnForPassBy`), so the caller feeds it other shooters' tracers only (the sandbox has none).
#[derive(Clone, Debug)]
pub struct PassByTracker { start: Vec3, direction: Vec3, prev_sign: f32, played: bool, radii: (f32, f32) }

fn sign(x: f32) -> f32 { if x > 0. { 1. } else if x < 0. { -1. } else { 0. } } // UKismetMathLibrary::SignOfFloat = FMath::Sign
fn map_range_clamped(v: f32, a: f32, b: f32, c: f32, d: f32) -> f32 { let t = if (b - a).abs() < 1e-12 { if v >= b { 1. } else { 0. } } else { ((v - a) / (b - a)).clamp(0., 1.) }; c + (d - c) * t }

impl PassByTracker {
    /// `OnInit(Start, End)`: `CachedDirectionVector = Start - End`, `bHasPlayedPassBySound = false`, one `TrackPassBy` (its result is not used) that primes `PreviousPlaneDotProd`.
    pub fn new(start: Vec3, end: Vec3, radii: (f32, f32), pawn: Vec3, tracer: Vec3) -> PassByTracker {
        let mut t = PassByTracker { start, direction: start - end, prev_sign: 0., played: false, radii };
        t.track(pawn, tracer);
        t
    }
    /// `TrackPassBy`: (changed, pass distance). The distance is that of the pawn from the bullet's line (`Start + project(pawn - Start, direction)`).
    fn track(&mut self, pawn: Vec3, tracer: Vec3) -> (bool, f32) {
        let s = sign((pawn - tracer).dot(self.direction));
        let changed = self.prev_sign != s;
        self.prev_sign = s;
        if !changed { return (false, 0.); }
        let d = self.direction;
        let proj = if d.length_squared() > 0. { d * ((pawn - self.start).dot(d) / d.length_squared()) } else { Vec3::ZERO };
        (true, (pawn - (self.start + proj)).length())
    }
    /// `ReceiveTick`: call once per tick with the local pawn's / the tracer's current locations and the pawn's `GetActorRightVector`.
    pub fn tick(&mut self, pawn: Vec3, pawn_right: Vec3, tracer: Vec3) -> Option<WhipSound> {
        let (changed, dist) = self.track(pawn, tracer);
        let intensity = 1. - map_range_clamped(dist, self.radii.0, self.radii.1, 0., 1.);
        if !(changed && !self.played && dist <= self.radii.1) { return None; }
        self.played = true;
        // PlayPassBySound: Location = pawn + RotateAngleAxis(right, degrees(dot(right.xy, normal(direction).xy)), Z) * PassByRadiusMax
        let n = self.direction.normalize_or_zero();
        let dot = Vec3::new(pawn_right.x, pawn_right.y, 0.).dot(Vec3::new(n.x, n.y, 0.));
        // (the Blueprint feeds the DOT PRODUCT through RadiansToDegrees into RotateAngleAxis, which converts it back: the angle in radians is the dot product itself)
        let (s, c) = dot.sin_cos();
        let r = pawn_right;
        let rot = Vec3::new(r.x * c - r.y * s, r.x * s + r.y * c, r.z); // rotation about the Z axis (UE RotateAngleAxis with axis (0,0,1))
        Some(WhipSound { location: pawn + rot * self.radii.1, far_volume: 1. - intensity, close_volume: intensity })
    }
}

#[cfg(test)]
mod weapon_passby_tests {
    use super::*;
    #[test] fn a_bullet_passing_the_pawn_whips_once_with_closeness_volumes() {
        // bullet flying along +X at y = 150 cm from the pawn (inside the 100..400 radii): start (0,150,0) -> end (4000,150,0)
        let (start, end) = (Vec3::new(0., 150., 0.), Vec3::new(4000., 150., 0.));
        let pawn = Vec3::ZERO;
        let mut t = PassByTracker::new(start, end, TRACER_PASS_BY_RADII, pawn, start);
        let mut got = vec![];
        for i in 0..100 { let tr = Vec3::new(i as f32 * 50., 150., 0.); if let Some(w) = t.tick(pawn, Vec3::new(0., 1., 0.), tr) { got.push((i, w)); } }
        assert_eq!(got.len(), 1, "{got:?}");
        let (_, w) = got[0];
        // closeness = 1 - MapRangeClamped(150, 100, 400, 0, 1) = 1 - 1/6
        assert!((w.close_volume - (1. - 50. / 300.)).abs() < 1e-5 && (w.far_volume - 50. / 300.).abs() < 1e-5, "{w:?}");
        assert!((w.location - pawn).length() > 399. && (w.location - pawn).length() < 401., "placed PassByRadiusMax from the pawn: {:?}", w.location);
    }
    #[test] fn a_bullet_beyond_the_max_radius_does_not_whip() {
        let (start, end) = (Vec3::new(0., 900., 0.), Vec3::new(4000., 900., 0.));
        let mut t = PassByTracker::new(start, end, TRACER_PASS_BY_RADII, Vec3::ZERO, start);
        for i in 0..100 { assert!(t.tick(Vec3::ZERO, Vec3::Y, Vec3::new(i as f32 * 50., 900., 0.)).is_none()); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::FrameInput;
    use fn_combat::Harvestable;

    fn run(g: &mut GameSim, d: &mut MomentDetector, table: Option<&StepTable>, frames: u32, f: impl Fn(u32) -> FrameInput) -> Vec<(u32, AudioMoment)> {
        let mut all = vec![];
        for i in 0..frames {
            g.step(&f(i), 1. / 60.);
            let mut o = vec![];
            d.observe(g, table, 1. / 60., &mut o);
            all.extend(o.into_iter().map(|m| (i, m)));
        }
        all
    }
    fn table() -> StepTable {
        // synthetic notifies at the same phases the real clips have (jog 0.6 s: right 0.27, left 0.57; sprint 0.4667 s: left 0.1973, right 0.4305)
        let mut v = vec![];
        for d in DIRECTIONS { v.push((format!("M_Jog_AssaultRifle_{d}_Core"), 0.6, vec![(0.2674, false), (0.5674, true)])); }
        v.push(("Sprint_TwoHanded".into(), 0.4667, vec![(0.1973, true), (0.4305, false)]));
        v.push(("CrouchSprint_THMelee".into(), 0.6, vec![(0.5741, true), (0.2462, false)]));
        for d in DIRECTIONS { v.push((format!("Crouch_ARNonTargeted_{d}_Core"), 0.9333, vec![(0.7911, true), (0.3359, false)])); }
        StepTable::from_clips(v)
    }

    #[test] fn families_drop_the_direction_and_gaits_follow_the_clip_name() {
        assert_eq!(family_key("M_Jog_AssaultRifle_NE_Core"), "M_Jog_AssaultRifle_Core");
        assert_eq!(family_key("Sprint_TwoHanded"), "Sprint_TwoHanded");
        assert_eq!(clip_gait("Sprint_TwoHanded"), Gait::Sprint);
        assert_eq!(clip_gait("CrouchSprint_THMelee"), Gait::CrouchSprint);
        assert_eq!(clip_gait("Crouch_ARNonTargeted_E_Core"), Gait::Crouch);
        assert_eq!(clip_gait("M_Walk_AR_N_Core"), Gait::Walk);
        let t = table();
        assert_eq!(t.families().len(), 4, "{:?}", t.families());
    }
    #[test] fn jump_and_landing_are_reported_once_with_the_impact_speed() {
        let mut g = GameSim::new(); let mut d = MomentDetector::new();
        let ev = run(&mut g, &mut d, None, 240, |f| FrameInput { jump: f == 30, ..Default::default() });
        let jumps: Vec<_> = ev.iter().filter(|e| matches!(e.1, AudioMoment::Jump)).collect();
        let lands: Vec<_> = ev.iter().filter_map(|e| if let AudioMoment::Land { speed } = e.1 { Some((e.0, speed)) } else { None }).collect();
        assert_eq!(jumps.len(), 1, "{ev:?}"); assert_eq!(lands.len(), 1, "{ev:?}");
        assert!(jumps[0].0 >= 30 && jumps[0].0 <= 33, "liftoff frame {}", jumps[0].0);
        assert!(lands[0].1 > 3. && lands[0].1 < 12., "impact {} m/s", lands[0].1);
    }
    #[test] fn a_swing_that_hits_a_tree_reports_swing_then_harvest_and_the_killing_hit_is_destroyed() {
        let mut g = GameSim::new(); g.world.props.clear(); g.world.props.push(Harvestable::new(Kind::Tree, Vec3::new(0., 0., 12. - 1.8)));
        let mut d = MomentDetector::new();
        let ev = run(&mut g, &mut d, None, 60 * 10, |_| FrameInput { attack: true, ..Default::default() });
        let swings = ev.iter().filter(|e| matches!(e.1, AudioMoment::Swing { .. })).count();
        let hits: Vec<_> = ev.iter().filter_map(|e| if let AudioMoment::Harvest { surface, destroyed, .. } = e.1 { Some((surface, destroyed)) } else { None }).collect();
        // 600 hit points / 50 damage per hit = 12 hits; the 12th is the killing one
        assert!(swings >= 12 && hits.len() == 12, "swings {swings} hits {} (swings after the tree is gone are misses)", hits.len());
        assert_eq!(hits.iter().filter(|h| h.1).count(), 1, "exactly one killing hit before the respawn: {hits:?}");
        assert!(hits[11].1, "the 12th hit kills: {hits:?}");
        assert!(hits.iter().all(|h| h.0 == HitSurface::Tree));
        let first = ev.iter().position(|e| matches!(e.1, AudioMoment::Harvest { .. })).unwrap();
        assert!(matches!(ev[first - 1].1, AudioMoment::Swing { .. }), "the swing comes first: {:?}", &ev[first - 1..=first]);
    }
    // CRACK AUDIO HOOK
    #[test] fn the_weak_spot_effect_becomes_spawn_level_and_impact_moments_without_draining_the_fx_events() {
        use fn_combat::harvest_fx::Aim;
        let mut g = GameSim::new(); g.world.props.clear(); g.world.props.push(Harvestable::new(Kind::Tree, Vec3::new(0., 0., -3.)));
        let mut d = MomentDetector::new();
        let obs = |g: &GameSim, d: &mut MomentDetector| { let mut o = vec![]; d.observe(g, None, 1. / 60., &mut o); o };
        assert!(obs(&g, &mut d).is_empty(), "the first observation only primes the detector");
        let cam = Vec3::new(0., 1.5, 2.);
        let aim_at = |eye: Vec3, t: Vec3| Aim { eye, dir: (t - eye).normalize() };
        // hit 1 (ordinary, generates the weak spot): spawn at the spot, then the Harvest moment of the hit
        g.world.fx.aim = Some(aim_at(cam, Vec3::new(0., 1.1, -3.))); assert!(g.world.strike(Vec3::ZERO, Vec3::NEG_Z));
        let spot = g.world.fx.weak_spot(0, &g.world.props[0]).expect("weak spot").pos;
        let o = obs(&g, &mut d);
        assert_eq!(o.iter().filter(|m| matches!(m, AudioMoment::CrackSpawn { pos } if *pos == spot)).count(), 1, "{o:?}");
        assert!(!o.iter().any(|m| matches!(m, AudioMoment::CrackLevel { .. } | AudioMoment::CrackImpact { .. })), "{o:?}");
        assert!(o.iter().any(|m| matches!(m, AudioMoment::Harvest { proto, .. } if *proto == fn_combat::NO_PROTO)), "{o:?}");
        // the presentation drains the queue once per frame: the audio must still see the next strike's events
        assert!(!g.world.fx.take_events().is_empty());
        // hits 2..: aim at the weak spot -> crack level 0, 1, 2, ... (CrackLevel; Sound_Crack_Level_01 first) + the crack impact, both at the OLD spot; no new spawn sound
        let mut levels = vec![];
        for _ in 0..3 {
            let s = g.world.fx.weak_spot(0, &g.world.props[0]).unwrap().pos;
            g.world.fx.aim = Some(aim_at(cam + Vec3::Y * 0.2, s)); assert!(g.world.strike(Vec3::ZERO, Vec3::NEG_Z));
            assert!(g.world.events.last().unwrap().weakpoint);
            let o = obs(&g, &mut d);
            let lv: Vec<_> = o.iter().filter_map(|m| if let AudioMoment::CrackLevel { level, pos } = m { Some((*level, *pos)) } else { None }).collect();
            assert_eq!(lv.len(), 1, "{o:?}"); assert_eq!(lv[0].1, s, "positional at the weak spot that was hit");
            assert_eq!(o.iter().filter(|m| matches!(m, AudioMoment::CrackImpact { pos } if *pos == s)).count(), 1, "{o:?}");
            assert!(!o.iter().any(|m| matches!(m, AudioMoment::CrackSpawn { .. })), "only the first generation spawns: {o:?}");
            levels.push(lv[0].0);
            if levels.len() == 2 { g.world.fx.take_events(); } // drained mid-way
        }
        assert_eq!(levels, vec![0, 1, 2], "the first weak-point hit is level 0 = Sound_Crack_Level_01");
        // an idle frame reports nothing
        assert!(obs(&g, &mut d).is_empty());
    }
    #[test] fn a_missed_swing_is_a_swing_without_a_harvest() {
        let mut g = GameSim::new(); g.world.props.clear(); let mut d = MomentDetector::new();
        let ev = run(&mut g, &mut d, None, 60, |f| FrameInput { attack: f == 3, ..Default::default() });
        assert_eq!(ev.iter().filter(|e| matches!(e.1, AudioMoment::Swing { .. })).count(), 1, "{ev:?}");
        assert!(!ev.iter().any(|e| matches!(e.1, AudioMoment::Harvest { .. })));
        let at = ev.iter().find(|e| matches!(e.1, AudioMoment::Swing { .. })).unwrap().0;
        assert!((at as f32 / 60. - 3. / 60. - 0.1603).abs() < 0.04, "TriggerAbility notify 0.1603 s after the press (frame {at})");
    }
    #[test] fn equip_is_reported_when_the_montage_starts() {
        let mut g = GameSim::new(); let mut d = MomentDetector::new();
        let ev = run(&mut g, &mut d, None, 10, |f| FrameInput { equip: f == 2, ..Default::default() });
        assert_eq!(ev.iter().filter(|e| matches!(e.1, AudioMoment::Equip)).count(), 1, "{ev:?}");
    }
    #[test] fn without_a_table_there_are_no_footsteps_and_with_one_they_follow_the_clips() {
        let mut g = GameSim::new(); let mut d = MomentDetector::new();
        let ev = run(&mut g, &mut d, None, 200, |_| FrameInput { forward: 1., ..Default::default() });
        assert!(!ev.iter().any(|e| matches!(e.1, AudioMoment::Step { .. })));
        let t = table();
        let mut g = GameSim::new(); let mut d = MomentDetector::new();
        let ev = run(&mut g, &mut d, Some(&t), 300, |_| FrameInput { forward: 1., ..Default::default() });
        let steps: Vec<_> = ev.iter().filter_map(|e| if let AudioMoment::Step { gait, left } = e.1 { Some((e.0, gait, left)) } else { None }).collect();
        assert!(steps.len() >= 6, "{steps:?}");
        assert!(steps.iter().all(|s| s.1 == Gait::Walk), "jog uses the walk cue: {steps:?}");
        for w in steps.windows(2) { assert_ne!(w[0].2, w[1].2, "feet alternate: {steps:?}"); }
    }
    #[test] fn sprinting_and_crouching_pick_their_gait_from_the_clip() {
        let t = table();
        let mut g = GameSim::new(); let mut d = MomentDetector::new();
        let ev = run(&mut g, &mut d, Some(&t), 300, |_| FrameInput { forward: 1., sprint_key: true, ..Default::default() });
        assert!(ev.iter().any(|e| matches!(e.1, AudioMoment::Step { gait: Gait::Sprint, .. })), "{:?}", ev.iter().filter(|e| matches!(e.1, AudioMoment::Step { .. })).collect::<Vec<_>>());
        let mut g = GameSim::new(); let mut d = MomentDetector::new();
        let ev = run(&mut g, &mut d, Some(&t), 300, |_| FrameInput { forward: 1., crouch_key: true, ..Default::default() });
        assert!(ev.iter().any(|e| matches!(e.1, AudioMoment::Step { gait: Gait::Crouch, .. })), "{:?}", ev.iter().filter(|e| matches!(e.1, AudioMoment::Step { .. })).collect::<Vec<_>>());
    }
    #[test] fn standing_still_and_airborne_make_no_steps() {
        let t = table();
        let mut g = GameSim::new(); let mut d = MomentDetector::new();
        let ev = run(&mut g, &mut d, Some(&t), 120, |_| FrameInput::default());
        assert!(!ev.iter().any(|e| matches!(e.1, AudioMoment::Step { .. })), "{ev:?}");
    }
}
