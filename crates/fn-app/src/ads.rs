//! ADS HOOK (fn-app): aim down sights. Right mouse (the `Target` binding, default RightMouseButton) holds / toggles (`Input.ToggleTargeting`) the
//! targeting state of the held firearm. The state machine, its timings and every number come from the 1.8 data (`fn_weapons::recovered_ads`, read by
//! `fn_pose::loadout_ads` into `Resources::ads`); this file connects them to the simulation:
//!
//! * `bIsTargeting` -> the animation graph (`GameSim::set_held`: `CoreAnims_Male[weapon]` Targeting pose / aim offset, crouch targeting pose, walk / crouch
//!   walk blend spaces and additives, `TargetingWeight` FInterpTo speed 12 - all already in `fn_animgraph`),
//! * the weapon's hold camera and targeting camera (`CameraBase3PClass` / `CameraTargeting3PClass` mode Blueprints, resolved over their class chains) ->
//!   `GameSim::ads` (blended eye + FOV: AR / shotgun keep FOV 80, the bolt sniper zooms to 15),
//! * the targeting speed limit (`GameSim::ads.speed_cap_cm`), look sensitivity (`MouseTargetingMultiplier` / `MouseScopedMultiplier` settings),
//! * the scope sequence outputs: pawn hidden once the timeline finished, scope amount (`DownSightPostProcessAmount`), reticle HUD hidden.
//!
//! The scope picture itself is the compiled `DownScopePostProcess` post-process material plus the `ScopeMesh1P` mesh: `crate::scope_hook` runs them through the
//! renderer's scope pass with [`Frame::scope_amount`] / [`Frame::scope_mesh`]. The ranged-weapon crosshair (`SFortReticle`, `fn_ui::screens::reticle`) is drawn by the HUD; `app.rs` hides it with
//! [`Frame::reticle_hidden`] (the recovered scope-timeline rule). The ZoomIn / ZoomOut sounds are counted in the machine, not played.
use fn_camera::{self as camera, Mode, Sample};
use fn_game::{FrameInput, GameSim};
use fn_movement::params as mp;
use fn_weapons::recovered_ads::{AdsInput, AdsMachine, AdsProfile, CameraModeData};

static SCRIPT_TARGET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_script_target(on: bool) { SCRIPT_TARGET.store(on, std::sync::atomic::Ordering::Relaxed); }
pub fn script_target() -> bool { SCRIPT_TARGET.load(std::sync::atomic::Ordering::Relaxed) }

fn leak_samples(v: &[(f32, [f32; 3])]) -> &'static [Sample] { Box::leak(v.iter().map(|(p, o)| Sample { pitch: *p, offset: *o }).collect::<Vec<_>>().into_boxed_slice()) }
/// A `fn_camera::Mode` from a resolved camera mode Blueprint (the penetration feelers are the Base class's, which no starter mode overrides).
pub fn mode_of(d: &CameraModeData) -> &'static Mode {
    Box::leak(Box::new(Mode {
        name: Box::leak(d.chain.first().cloned().unwrap_or_default().into_boxed_str()), fov: d.fov, forward_distance: d.forward_distance, origin_interp: d.origin_interp,
        vt_offset: leak_samples(&d.vt_offset), vt_offset_crouch: leak_samples(&d.vt_offset_crouch), vt_interp: d.vt_interp, cs_offset: leak_samples(&d.cs_offset),
        push_out: d.push_out, hide_pawn_pct: d.hide_pawn_pct, transition_time: d.transition_in, feelers: &camera::FEELERS,
    }))
}

/// What one frame of ADS produced.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub aiming: bool, pub weight: f32, pub camera_active: bool, pub pawn_hidden: bool,
    pub speed_cap_cm: Option<f32>,
    /// Multiplier for this frame's look deltas (1 = not targeting).
    pub look_scale: f32,
    /// `DownSightPostProcessAmount` (scope post-process blend weight) and the other scope outputs.
    pub scope_amount: f32, pub scope_mesh: bool, pub reticle_hidden: bool,
}

pub struct AdsDriver {
    pub profiles: Vec<AdsProfile>,
    modes: Vec<(&'static Mode, &'static Mode)>,
    pub machine: AdsMachine,
    gun: Option<usize>,
    /// Test aid (never set in play): `FNRT_ADS=1` holds the Target binding down.
    force: bool,
    was_aiming: bool, zoom_seen: (u32, u32),
}
impl AdsDriver {
    /// One profile per starter firearm (`Resources::ads`); None when the data could not be read.
    pub fn from_resources(r: &fn_pose::loadout::Resources) -> Option<AdsDriver> {
        if r.ads.len() < fn_pose::loadout_gun::STARTER_GUNS { return None; }
        let modes = r.ads.iter().map(|p| (mode_of(&p.base), mode_of(&p.targeting))).collect();
        for p in &r.ads { eprintln!("[ads] {}: hold camera {} (FOV {}), ADS camera {} (FOV {}, blend in {:.2}s out {:.2}s){}", p.item.rsplit('/').next().unwrap_or(&p.item), p.base.chain[0], p.base.fov, p.targeting.chain[0], p.targeting.fov,
            p.targeting.blend.in_duration(p.targeting.transition_in), p.targeting.transition_out, if p.use_scope { ", scope (post-process material + scope mesh: scope_hook)" } else { "" }); }
        Some(AdsDriver { profiles: r.ads.clone(), modes, machine: AdsMachine::new(), gun: None, force: std::env::var_os("FNRT_ADS").is_some(), was_aiming: false, zoom_seen: (0, 0) })
    }
    pub fn new(profiles: Vec<AdsProfile>) -> AdsDriver {
        let modes = profiles.iter().map(|p| (mode_of(&p.base), mode_of(&p.targeting))).collect();
        AdsDriver { profiles, modes, machine: AdsMachine::new(), gun: None, force: false, was_aiming: false, zoom_seen: (0, 0) }
    }
    /// Appends the profile of a firearm installed after start-up (gun index = the previous `profiles.len()`).
    pub fn add_profile(&mut self, p: AdsProfile) {
        eprintln!("[ads] {}: hold camera {} (FOV {}), ADS camera {} (FOV {}){}", p.item.rsplit('/').next().unwrap_or(&p.item), p.base.chain[0], p.base.fov, p.targeting.chain[0], p.targeting.fov, if p.use_scope { ", scope" } else { "" });
        self.modes.push((mode_of(&p.base), mode_of(&p.targeting)));
        self.profiles.push(p);
    }
    pub fn profile(&self) -> Option<&AdsProfile> { self.gun.and_then(|g| self.profiles.get(g)) }

    /// Advances the machine for the held firearm (`gun` = firearm index 0..3, None = pickaxe / consumable) and returns this frame's outputs.
    pub fn step(&mut self, dt: f32, held: bool, toggle: bool, gun: Option<usize>, can_aim: bool, mouse_ads: f32, mouse_scope: f32) -> Frame {
        if gun != self.gun { self.machine.reset(); self.gun = gun; self.was_aiming = false; }
        let p = gun.and_then(|g| self.profiles.get(g));
        self.machine.update(dt, AdsInput { held: held || self.force, can_aim, toggle }, p);
        let Some(p) = p else { return Frame { look_scale: 1., ..Default::default() } };
        let m = &self.machine;
        if m.is_targeting() != self.was_aiming { self.was_aiming = m.is_targeting(); eprintln!("[ads] {} {}", p.item.rsplit('/').next().unwrap_or(&p.item), if self.was_aiming { "targeting ON" } else { "targeting OFF" }); }
        if (m.zoom_in_events, m.zoom_out_events) != self.zoom_seen { self.zoom_seen = (m.zoom_in_events, m.zoom_out_events); eprintln!("[ads] scope timeline {} (ZoomIn x{} / ZoomOut x{}; sounds are not played)", if m.timeline_pos() > 0. || m.is_targeting() { "forward" } else { "reverse" }, m.zoom_in_events, m.zoom_out_events); }
        Frame { aiming: m.is_targeting(), weight: m.camera_weight(p), camera_active: m.camera_active(), pawn_hidden: m.pawn_hidden(), speed_cap_cm: m.limits_movement().then_some(fn_live::f32("FN_MOVE_SPEED_WALK", mp::SPEED_WALK)),
            look_scale: m.look_scale(p, mouse_ads, mouse_scope), scope_amount: m.scope_amount(p), scope_mesh: m.scope_mesh_visible(), reticle_hidden: m.reticle_hidden() }
    }
    /// Hands the frame to the simulation: the held weapon's camera modes + blend, speed limit and pawn hiding; the look deltas of `inp` are scaled.
    /// The MOUSE targeting / scoped multiplier (`f.look_scale`; vfunc_71 +0x4f4 / +0x4f8) is applied here; the right stick has its own pair (+0x4fc / +0x500)
    /// applied before its look curve (fn_game::gamepad_look) and is added to `inp` after this. The absolute
    /// `FOV * FOVScale` term of the mouse (UFortPlayerInputAthena vfunc_71 @1404392c0, ini `FOVScale=0.01111`) is already in the mouse part of
    /// `inp` (`InputState::gather` with the last view's FOV), so scripted `turn=` degrees stay degrees. `_mouse` is kept for the callers.
    pub fn apply(&self, f: &Frame, sim: &mut GameSim, inp: &mut FrameInput, _mouse: bool) {
        let modes = self.gun.and_then(|g| self.modes.get(g));
        sim.ads.base = modes.map(|m| m.0); sim.ads.target = modes.map(|m| m.1);
        sim.ads.weight = f.weight; sim.ads.active = f.camera_active; sim.ads.hide_pawn = f.pawn_hidden; sim.ads.speed_cap_cm = f.speed_cap_cm;
        if sim.dbno { sim.ads.base = Some(&fn_camera::DBNO); sim.ads.target = None; sim.ads.weight = 0.; sim.ads.active = false; sim.ads.hide_pawn = false; sim.ads.speed_cap_cm = None; }
        let k = if f.aiming { f.look_scale } else { 1. };
        inp.yaw_delta *= k; inp.pitch_delta *= k;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fn_pose::{loadout_ads, ClipSourceDir};
    fn profiles() -> Vec<AdsProfile> {
        let root = fn_live::var_os("FN_TEST_ROOT").expect("set FN_TEST_ROOT to the local 1.8 install");
        let set = fn_pak::PakSet::open_install(root).unwrap();
        loadout_ads::load(&ClipSourceDir::Pak(&set), &fn_pose::loadout::ITEMS[..3]).unwrap()
    }
    fn near(a: f32, b: f32) -> bool { (a - b).abs() < 1e-4 }

    #[test] fn a_resolved_blueprint_chain_reproduces_the_verified_melee_mode_constant() {
        let root = fn_live::var_os("FN_TEST_ROOT").expect("set FN_TEST_ROOT to the local 1.8 install");
        let set = fn_pak::PakSet::open_install(root).unwrap();
        let m = mode_of(&loadout_ads::camera_mode(&ClipSourceDir::Pak(&set), "/Game/Blueprints/Camera/Athena/Athena_PlayerCameraModeMelee").unwrap());
        let r = &camera::MELEE;
        assert_eq!((m.fov, m.forward_distance, m.origin_interp, m.vt_interp, m.push_out, m.hide_pawn_pct, m.transition_time), (r.fov, r.forward_distance, r.origin_interp, r.vt_interp, r.push_out, r.hide_pawn_pct, r.transition_time));
        let eq = |a: &[Sample], b: &[Sample]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.pitch == y.pitch && x.offset == y.offset);
        assert!(eq(m.vt_offset, r.vt_offset) && eq(m.vt_offset_crouch, r.vt_offset_crouch) && eq(m.cs_offset, r.cs_offset));
    }

    /// Whole chain on the real loadout: aim with the sniper on the headless sim for 1 s, then release.
    #[test] fn sniper_ads_zooms_to_15_hides_the_pawn_limits_speed_and_releases() {
        let mut d = AdsDriver::new(profiles());
        let mut sim = GameSim::new();
        let walk = |sim: &mut GameSim, d: &mut AdsDriver, secs: f32, held: bool| -> Frame {
            let mut f = Frame::default();
            for _ in 0..(secs * 60.) as usize {
                let mut inp = FrameInput { forward: 1., yaw_delta: 1., ..Default::default() };
                f = d.step(1. / 60., held, false, Some(2), true, 0.3, 0.6); d.apply(&f, sim, &mut inp, true);
                assert!(!f.aiming || near(inp.yaw_delta, 0.6), "the Mouse Scope slider scales the look rate: {}", inp.yaw_delta);
                sim.set_held(Some(("AssaultRifle", f.aiming)));
                sim.step(&inp, 1. / 60.);
            }
            f
        };
        // hold camera: FOV 80, speed = run stat
        let f = walk(&mut sim, &mut d, 2., false); assert!(!f.aiming && sim.ads.fov == 80. && !sim.cam_view.hide_pawn);
        let run_speed = sim.controller.velocity.length(); assert!(run_speed > 3.5, "run {run_speed} m/s");
        // press: FOV zooms, speed falls to the walk stat (200 cm/s), anim graph sees bIsTargeting
        let f = walk(&mut sim, &mut d, 0.2, true); assert!(f.aiming && f.weight > 0. && sim.ads.fov < 80. && sim.ads.fov > 15., "mid-blend fov {}", sim.ads.fov); assert!(sim.anim.vars.b_is_targeting);
        let f = walk(&mut sim, &mut d, 1.5, true);
        assert!(f.aiming && f.weight == 1. && near(sim.ads.fov, 15.), "fov {}", sim.ads.fov);
        assert!(f.pawn_hidden && f.scope_mesh && f.reticle_hidden && f.scope_amount == 1. && sim.cam_view.hide_pawn, "scope timeline finished: {f:?}");
        let s = sim.controller.velocity.length(); assert!(s <= 2.05 && s > 1.5, "walk speed while targeting {s} m/s");
        assert!(sim.anim.vars.targeting_weight > 0.99);
        // release: camera and FOV return, pawn shown again after the stop delay, speed restored
        let f = walk(&mut sim, &mut d, 0.15, false); assert!(!f.aiming && !f.pawn_hidden, "{f:?}");
        let f = walk(&mut sim, &mut d, 1.5, false); assert!(f.weight == 0. && !f.camera_active && !f.scope_mesh && sim.ads.fov == 80. && sim.controller.velocity.length() > 3.5);
    }
    #[test] fn ar_ads_keeps_fov_80_and_only_moves_the_camera() {
        let mut d = AdsDriver::new(profiles()); let mut sim = GameSim::new();
        let eye_at = |sim: &mut GameSim, d: &mut AdsDriver, held: bool| { for _ in 0..90 { let mut inp = FrameInput::default(); let f = d.step(1. / 60., held, false, Some(0), true, 0.5, 0.5); d.apply(&f, sim, &mut inp, true); sim.step(&inp, 1. / 60.); } (sim.cam_view.eye - sim.feet(), sim.ads.fov) };
        let (hold, f0) = eye_at(&mut sim, &mut d, false); let (ads, f1) = eye_at(&mut sim, &mut d, true);
        assert_eq!((f0, f1), (80., 80.)); assert!((ads - hold).length() > 0.3, "the targeting camera sits closer / more over the shoulder: {hold:?} -> {ads:?}");
        assert!(sim.ads.hide_pawn == false, "only the scope hides the pawn");
    }
    #[test] fn switching_weapons_drops_aiming() {
        let mut d = AdsDriver::new(profiles());
        for _ in 0..30 { d.step(1. / 60., true, false, Some(0), true, 0.5, 0.5); } assert!(d.machine.is_targeting());
        let f = d.step(1. / 60., true, false, None, false, 0.5, 0.5); assert!(!f.aiming && f.look_scale == 1.);
    }
}
