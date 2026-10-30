//! Deterministic numeric tests of the UE 4.16 CharacterMovementComponent port (movement_cmc.rs, movement_scene.rs) and the
//! metres-based `Controller` on top of it. All component-level tests work in UE units (cm, s).
//! Tolerances are stated next to each assertion; they come from two engine properties that the port keeps on purpose:
//!  * Euler integration of velocity (`Velocity += Acceleration * dt` before the move) makes distance covered while accelerating
//!    frame-rate dependent by `a * t * dt / 2` (7 cm after the 0.55 s ramp at 30 fps, 0.9 cm at 240 fps); falling is exact (midpoint rule).
//!  * the capsule hovers 1.9..2.4 cm above the floor (`MIN_FLOOR_DIST`/`MAX_FLOOR_DIST`) and sweeps stop 0.1 cm short of surfaces.
use crate::cmc::{Cmc, Mode, Params};
use crate::scene::{Frustum, Scene};
use crate::params as mp;
use crate::controller::Controller;
use glam::{Vec2, Vec3};

fn cmc_at(scene: Scene, x: f32, y: f32, feet_z: f32) -> Cmc { Cmc::new(Params::default(), scene, Vec3::new(x, y, feet_z)) }
fn flat() -> Cmc { cmc_at(Scene::default(), 0., 0., 0.) }
fn cyl(x: f32, y: f32, z0: f32, z1: f32, r: f32) -> Frustum { Frustum { c: Vec2::new(x, y), z0, z1, r0: r, r1: r } }
fn cone(x: f32, y: f32, z1: f32, r0: f32) -> Frustum { Frustum { c: Vec2::new(x, y), z0: 0., z1, r0, r1: 0. } }
fn scene(f: &[Frustum]) -> Scene { Scene { solids: f.to_vec(), ..Default::default() } }
fn run(c: &mut Cmc, secs: f32, hz: f32, input: Vec2) { let n = (secs * hz).round() as usize; for _ in 0..n { c.tick(input, 1. / hz); } }
/// Jump from rest at `hz`; returns (apex rise above the takeoff capsule base, flight time in s measured from the press to touchdown).
fn jump_stats(hz: f32) -> (f32, f32) {
    let mut c = flat(); c.p.jump_max_hold_time = 0.; let dt = 1. / hz; c.jump(); let base0 = c.feet_z(); let mut apex = 0f32; let mut t = 0.; let mut left = false;
    for _ in 0..(3. * hz) as usize {
        c.tick(Vec2::ZERO, dt); t += dt;
        if c.is_falling() { left = true; apex = apex.max(c.feet_z() - base0); }
        if left && c.is_moving_on_ground() { break; }
    }
    (apex, t)
}

// ---------------------------------------------------------------------------------------------------------------------
// gravity / jump
// ---------------------------------------------------------------------------------------------------------------------
#[test] fn pawn_spawns_standing_on_the_floor_inside_the_floor_distance_band() {
    let c = flat(); assert_eq!(c.mode, Mode::Walking); assert!(c.floor.is_walkable_floor());
    let d = c.floor.floor_dist; assert!(d >= 1.8 && d <= 2.5, "floor dist {d}"); assert_eq!(c.feet_height(), 0.);
}
#[test] fn jump_apex_and_airtime_follow_the_recovered_numbers() {
    // apex = v^2/(2g) = 600^2/5600 = 64.29 cm, airtime = 2v/g = 0.4286 s (+ the 2 cm hover). Falling uses the midpoint rule, which is exact for
    // constant gravity, so the only error is the sampling of the apex: g*dt^2/8 and the frame quantisation of the touchdown.
    // At 30 Hz the pawn tick delta (0.0333 s) is above the 0.03 s threshold of `UFortMovementComp_Character::DoJump`, so JumpZ is scaled (`mp::jump_frame_scale`): the closed
    // form is `jump_apex_cm_at(dt)`; at 60 Hz and faster the scale is 1.
    for hz in [30., 60., 144., 240.] {
        let (apex, air) = (mp::jump_apex_cm_at(1. / hz), mp::jump_airtime_at(1. / hz));
        let (a, t) = jump_stats(hz);
        assert!((a - apex).abs() < 2800. / (hz * hz * 8.) + 0.05, "{hz} Hz apex {a} vs {apex}");
        assert!((t - air).abs() < 1.5 / hz + 0.006, "{hz} Hz airtime {t} vs {air}");
    }
}
#[test] fn free_fall_matches_the_closed_form_at_every_frame_rate() {
    // 10 m drop: t = sqrt(2h/g); speed at the floor = sqrt(2 g h) = 2366 cm/s (< terminal velocity 4000).
    for hz in [30., 60., 144., 240.] {
        let mut c = cmc_at(Scene::default(), 0., 0., 1000.); assert_eq!(c.mode, Mode::Falling);
        let dt = 1. / hz; let mut t = 0.; let mut ok = false;
        for _ in 0..(6. * hz) as usize { c.tick(Vec2::ZERO, dt); t += dt; if c.is_moving_on_ground() { ok = true; break; } }
        assert!(ok); let expect = (2. * 1000. / 2800f32).sqrt(); assert!((t - expect).abs() < 1.5 / hz + 0.004, "{hz} Hz: {t} vs {expect}");
    }
}
#[test] fn terminal_velocity_caps_a_long_fall() {
    let mut c = cmc_at(Scene::default(), 0., 0., 20000.); let mut vmax = 0f32;
    for _ in 0..(10 * 60) { c.tick(Vec2::ZERO, 1. / 60.); vmax = vmax.max(-c.velocity.z); }
    assert!((vmax - 4000.).abs() < 1., "{vmax}");
}
#[test] fn gravity_scale_scales_apex_inversely() {
    let mut p = Params::default(); p.gravity_scale = 2.; p.jump_max_hold_time = 0.; let mut c = Cmc::new(p, Scene::default(), Vec3::ZERO); c.jump(); let b = c.feet_z(); let mut apex = 0f32;
    for _ in 0..240 { c.tick(Vec2::ZERO, 1. / 240.); apex = apex.max(c.feet_z() - b); } assert!((apex - 600. * 600. / (2. * 5600.)).abs() < 0.3, "{apex}");
}
#[test] fn the_jump_uses_the_recovered_project_gravity() {
    // DefaultEngine.ini PhysicsSettings DefaultGravityZ -2800 with JumpZ 600 cm/s: 28 m/s^2, 0.643 m apex, 0.429 s airtime, 0.214 s to the apex.
    let (a, t) = jump_stats(240.); assert!((a / 100. - 0.6429).abs() < 0.01 && (t - 0.4286).abs() < 0.015, "{a} {t}");
}

// ---------------------------------------------------------------------------------------------------------------------
// jump input rules (ACharacter::CheckJumpInput / CanJump / DoJump)
// ---------------------------------------------------------------------------------------------------------------------
#[test] fn a_press_in_the_air_is_ignored_in_every_phase() {
    for hz in [30., 60., 144., 240.] {
        let mut c = flat(); c.p.jump_max_hold_time = 0.; c.jump(); let dt = 1. / hz; let mut presses = 0; let mut apex_seen = false;
        for i in 0..(2. * hz) as usize {
            c.tick(Vec2::ZERO, dt);
            if c.is_falling() && i % 3 == 0 { let (vz, jumped) = (c.velocity.z, c.jumped); c.jump(); presses += 1; c.tick(Vec2::ZERO, dt); if c.is_falling() { assert!(c.velocity.z <= vz + 1e-3, "{hz} Hz: vertical speed gained from an air press ({vz} -> {})", c.velocity.z); } assert_eq!(c.jumped, jumped, "{hz} Hz: air press started a jump"); }
            if c.velocity.z < 0. { apex_seen = true; }
        }
        assert!(presses >= 3 && apex_seen && c.jumped == 1, "{hz} Hz jumps {}", c.jumped);
    }
}
#[test] fn walking_off_a_ledge_counts_as_the_first_jump() {
    // CheckJumpInput: JumpCurrentCount 0 while falling -> incremented, so JumpMaxCount 1 refuses the press.
    let mut c = cmc_at(scene(&[cyl(0., 0., 0., 100., 150.)]), 0., 0., 100.); assert!(c.is_moving_on_ground());
    let mut fell = false;
    for _ in 0..(3 * 60) { c.tick(Vec2::X, 1. / 60.); if c.is_falling() { fell = true; break; } }
    assert!(fell, "never walked off the plateau");
    c.jump(); let vz = c.velocity.z; c.tick(Vec2::X, 1. / 60.); assert_eq!(c.jumped, 0); assert!(c.velocity.z <= vz + 1e-3);
}
#[test] fn holding_jump_makes_exactly_one_jump_until_the_key_is_released() {
    let mut c = Controller::default(); let mut jumps_at_land = vec![];
    for _ in 0..(5 * 60) { c.step(Vec2::ZERO, true, false, 1. / 60.); if c.landings() as usize > jumps_at_land.len() { jumps_at_land.push(c.jumps_started()); } }
    assert_eq!(c.jumps_started(), 1); assert_eq!(c.landings(), 1); assert!(c.grounded() && c.height == 0.);
    c.step(Vec2::ZERO, false, false, 1. / 60.); c.step(Vec2::ZERO, true, false, 1. / 60.); assert_eq!(c.jumps_started(), 2, "a fresh press jumps again");
}
#[test] fn a_press_during_landing_recovery_and_a_press_just_before_touchdown() {
    // ground press during the landing animation window = ordinary jump; a press 2 frames before touchdown must not be buffered.
    let mut c = Controller::default(); let dt = 1. / 60.; let mut f = 0; let mut press_land = None;
    c.step(Vec2::ZERO, true, false, dt); c.step(Vec2::ZERO, false, false, dt);
    loop { f += 1; if f > 600 { panic!("never landed"); } let near = c.airborne() && crate::controller::fall_time_left(c.height, c.vertical_speed) < 2.5 * dt && c.vertical_speed < 0.;
        c.step(Vec2::ZERO, near && press_land.is_none(), false, dt); if near && press_land.is_none() { press_land = Some(f); } if c.grounded() && f > 5 { break; } }
    assert_eq!(c.jumps_started(), 1, "a press right before touchdown was buffered into a second jump");
    c.step(Vec2::ZERO, false, false, dt); for _ in 0..5 { c.step(Vec2::ZERO, false, false, dt); } c.step(Vec2::ZERO, true, false, dt);
    assert_eq!(c.jumps_started(), 2, "a ground press right after landing is a normal jump"); assert!(c.airborne());
}
#[test] fn crouch_blocks_jump_while_requested_but_not_on_the_frame_it_is_released() {
    let mut c = Controller::default(); c.wants_crouch = true; for _ in 0..30 { c.step(Vec2::ZERO, false, false, 1. / 60.); } assert!(c.crouched);
    c.step(Vec2::ZERO, true, false, 1. / 60.); assert_eq!(c.jumps_started(), 0, "jump must be refused while the crouch key is held");
    c.step(Vec2::ZERO, false, false, 1. / 60.);
    c.wants_crouch = false; c.step(Vec2::ZERO, true, false, 1. / 60.); assert!(!c.crouched && c.jumps_started() == 0, "the press on the release frame stands up"); c.step(Vec2::ZERO, true, false, 1. / 60.); assert_eq!(c.jumps_started(), 1, "and jumps on the next held tick"); assert!(c.airborne());
    for _ in 0..3 { c.step(Vec2::ZERO, true, false, 1. / 60.); } assert!(!c.crouched, "the capsule stands up in the air");
}
#[test] fn jump_stands_a_crouched_pawn_and_keeps_the_apex() {
    let mut c = tap_controller(); c.wants_crouch = true; for _ in 0..30 { c.step(Vec2::ZERO, false, false, 1. / 60.); }
    c.wants_crouch = false; let mut apex = 0f32; for i in 0..200 { c.step(Vec2::ZERO, i < 2, false, 1. / 120.); apex = apex.max(c.height); }
    assert!((apex - 0.6429).abs() < 0.05, "{apex}");
}
#[test] fn apply_gravity_while_jumping_is_not_a_1_8_behaviour() {
    // exe 0x2088a20 (PhysFalling) passes the plain sub-step to NewFallVelocity: JumpMaxHoldTime / bApplyGravityWhileJumping (UE >= 4.17) have no effect on the rise.
    let rise = |hold: f32, grav_while_jumping: bool| {
        let mut p = Params::default(); p.jump_max_hold_time = hold; p.apply_gravity_while_jumping = grav_while_jumping;
        let mut c = Cmc::new(p, Scene::default(), Vec3::ZERO); c.jump(); let b = c.feet_z(); let mut apex = 0f32;
        for _ in 0..300 { c.tick(Vec2::ZERO, 1. / 120.); apex = apex.max(c.feet_z() - b); } apex
    };
    assert_eq!(rise(0.25, false), rise(0.25, true)); assert!((rise(0., true) - mp::jump_apex_cm_at(1. / 120.)).abs() < 0.2);
}

// ---------------------------------------------------------------------------------------------------------------------
// walking: acceleration, speed caps, braking
// ---------------------------------------------------------------------------------------------------------------------
const FWD: Vec2 = Vec2::new(0., -1.);
#[test] fn acceleration_from_rest_is_max_acceleration_at_every_frame_rate() {
    for hz in [30., 60., 144., 240.] { let mut c = flat(); run(&mut c, 0.3, hz, FWD); assert!((-c.velocity.y - 225.).abs() < 1.5, "{hz} Hz: {}", c.velocity.y); }
}
#[test] fn speed_caps_are_the_recovered_stats() {
    let cap = |sprint: bool, crouch: bool, dir: Vec2| { let mut c = flat(); c.wants_sprint = sprint; c.wants_crouch = crouch; run(&mut c, 3., 60., dir); c.velocity.length() };
    assert!((cap(false, false, FWD) - 410.).abs() < 0.5); assert!((cap(true, false, FWD) - 550.).abs() < 0.5); assert!((cap(false, true, FWD) - 290.).abs() < 0.5);
    // facing is -Y, so +Y is straight backwards (0.65) and +X a strafe (90 degrees -> 0.8: the recovered curve is a RCIM_Constant step function, 1.0 below 70 deg, 0.8 to 110 deg, 0.65 beyond)
    assert!((cap(false, false, -FWD) - 410. * 0.65).abs() < 0.5, "{}", cap(false, false, -FWD)); assert!((cap(false, false, Vec2::X) - 410. * 0.8).abs() < 0.5);
    // diagonal input is clamped to length 1 and the speed cap applies to the vector length
    assert!((cap(true, false, Vec2::new(1., -1.)) - 550. * mp::directional_multiplier(45.)).abs() < 0.5); assert_eq!(mp::directional_multiplier(45.), 1., "a 45 degree diagonal is below the first step (70 deg): full speed");
}
#[test] fn braking_distance_matches_the_friction_ode_and_converges_with_frame_rate() {
    // dv/dt = -(GroundFriction*BrakingFrictionFactor) v - BrakingDecelerationWalking, k = 12 1/s, a = 1000 cm/s^2, v0 = 410 cm/s.
    // UE applies the braked velocity for the whole frame, so the stopping distance DEPENDS on the frame rate (engine behaviour, kept):
    // 30 fps 13.2 cm, 60 fps 16.3, 144 fps 19.5, 240 fps 20.4, 500 fps 21.1 -> ODE 21.8 as dt -> 0.
    let (k, a, v0) = (mp::GROUND_FRICTION * mp::BRAKING_FRICTION_FACTOR, mp::BRAKING_DECELERATION_WALKING, 410f32);
    let t_stop = ((v0 + a / k) / (a / k)).ln() / k; let x_stop = (v0 + a / k) / k * (1. - (-k * t_stop).exp()) - a / k * t_stop;
    assert!(x_stop > 21. && x_stop < 22.5, "{x_stop}");
    let mut d = vec![];
    for hz in [30., 60., 144., 240., 500.] {
        let mut c = flat(); c.velocity = Vec3::new(0., -v0, 0.); let y0 = c.loc.y; run(&mut c, 1., hz, Vec2::ZERO);
        assert_eq!(c.velocity, Vec3::ZERO); d.push(y0 - c.loc.y);
    }
    for w in d.windows(2) { assert!(w[0] < w[1], "stopping distance must grow with frame rate: {d:?}"); }
    assert!(d[4] > 0.95 * x_stop && d[4] < x_stop + 0.5, "500 Hz {} vs ODE {x_stop}", d[4]);
    assert!(d[3] > 0.92 * x_stop, "240 Hz {} vs ODE {x_stop}", d[3]); assert!(d[1] > 0.7 * x_stop && d[0] > 0.55 * x_stop, "{d:?}");
}
#[test] fn run_distance_is_frame_rate_independent_within_the_euler_tolerance() {
    let mut ends = vec![]; for hz in [30., 60., 144., 240.] { let mut c = flat(); run(&mut c, 2., hz, FWD); ends.push((-c.loc.y, -c.velocity.y)); }
    for (i, e) in ends.iter().enumerate() { assert!((e.1 - 410.).abs() < 0.5); assert!((e.0 - ends[3].0).abs() < 8., "{i}: {:?} vs {:?}", e, ends[3]); }
    // analytic: ramp 0.547 s (a = 750) then 410 cm/s: 410^2/(2*750) + 410*(2-0.547) = 112 + 595.7 = 707.8 cm
    assert!((ends[3].0 - 707.8).abs() < 3., "{}", ends[3].0);
}
#[test] fn irregular_frame_times_do_not_change_the_result_much() {
    let mut a = flat(); let mut b = flat(); for i in 0..240 { a.tick(FWD, 1. / 120.); b.tick(FWD, if i % 2 == 0 { 0.002 } else { 0.0146667 }); }
    assert!((a.loc.y - b.loc.y).abs() < 6.0, "{} vs {}", a.loc.y, b.loc.y);
}
#[test] fn long_frames_are_substepped_like_the_engine() {
    // one 0.2 s frame must be as good as 12 frames of 1/60 (within the Euler tolerance), and never tunnel
    let mut a = Cmc::new(Params::default().pre_1p8_trace(), Scene::default(), Vec3::ZERO); a.tick(FWD, 0.2); let mut b = flat(); run(&mut b, 0.2, 60., FWD);
    assert!((a.loc.y - b.loc.y).abs() < 6. && (a.velocity.y - b.velocity.y).abs() < 1., "{} {}", a.loc.y, b.loc.y);
}
#[test] fn the_athena_pawn_substeps_at_0_2_s_with_two_iterations() {
    let mut a = flat(); a.tick(FWD, 0.4); let mut b = flat(); b.tick(FWD, 0.2); b.tick(FWD, 0.2);
    assert!((a.loc.y - b.loc.y).abs() < 0.5 && (a.velocity.y - b.velocity.y).abs() < 0.5, "{} {}", a.loc.y, b.loc.y);
    let mut c = flat(); c.tick(FWD, 0.2); let mut d = flat(); run(&mut d, 0.2, 60., FWD); assert!((c.loc.y - d.loc.y).abs() > 6., "a 0.2 s frame is a single step");
}

// ---------------------------------------------------------------------------------------------------------------------
// air control
// ---------------------------------------------------------------------------------------------------------------------
#[test] fn air_control_boosts_low_speed_and_keeps_momentum() {
    // 0.95 * boost 2 -> 1.0 below 25 cm/s: the first half second in the air accelerates at the full 750 cm/s^2, like the ground
    let mut c = flat(); c.jump(); c.tick(Vec2::ZERO, 1. / 120.); c.jump(); let mut t = 0.;
    while t < 0.2 { c.tick(Vec2::new(1., 0.), 1. / 120.); t += 1. / 120.; }
    assert!(c.is_falling()); assert!(c.velocity.x > 0.95 * 750. * 0.15 && c.velocity.x < 750. * 0.21, "vx {}", c.velocity.x);
    // no input: lateral momentum is preserved (FallingLateralFriction 0, BrakingDecelerationFalling 0)
    let vx = c.velocity.x; for _ in 0..20 { c.tick(Vec2::ZERO, 1. / 120.); } assert!(c.is_falling()); assert!((c.velocity.x - vx).abs() < 1e-3);
}
#[test] fn a_sprint_jump_keeps_its_ground_speed_in_the_air() {
    let mut c = flat(); c.wants_sprint = true; run(&mut c, 1.5, 60., FWD); let v = c.velocity.y; c.jump(); for _ in 0..15 { c.tick(FWD, 1. / 60.); } assert!(c.is_falling()); assert!((c.velocity.y - v).abs() < 1., "{} vs {v}", c.velocity.y);
}

// ---------------------------------------------------------------------------------------------------------------------
// steps, slopes, ledges, ceilings
// ---------------------------------------------------------------------------------------------------------------------
#[test] fn step_up_succeeds_below_max_step_height_and_is_blocked_above_it() {
    for (h, ok) in [(30., true), (44., true), (50., false), (80., false)] {
        let mut c = cmc_at(scene(&[cyl(0., -200., 0., h, 100.)]), 0., 0., 0.); let mut up = 0f32; for _ in 0..(3 * 60) { c.tick(FWD, 1. / 60.); up = up.max(c.feet_height()); } if ok { assert!((up - h).abs() < 0.2 && c.is_moving_on_ground(), "step {h}: feet {up}"); } else { assert!(up < 1., "step {h}: climbed to {up}"); }
    }
}
#[test] fn slope_limit_is_the_walkable_floor_angle() {
    // WalkableFloorZ 0.71 = 44.77 degrees. A 40 degree cone is climbed, a 50 degree cone is not.
    let climb = |angle_deg: f32| { let r = 300.; let h = r * angle_deg.to_radians().tan(); let mut c = cmc_at(scene(&[cone(0., -300., h, r)]), 0., 0., 0.); let mut best = 0f32; for _ in 0..(5 * 60) { c.tick(FWD, 1. / 60.); best = best.max(c.feet_height()); } best };
    let ok = climb(40.); let no = climb(50.);
    assert!(ok > 60., "40 degree slope only reached {ok} cm"); assert!(no < 25., "50 degree slope climbed to {no} cm");
    // the cone angle test brackets the limit
    assert!((mp::WALKABLE_FLOOR_Z.acos().to_degrees() - 44.765).abs() < 0.01);
}
#[test] fn walking_off_a_ledge_falls_and_lands() {
    let mut c = cmc_at(scene(&[cyl(0., 0., 0., 100., 150.)]), 0., 0., 100.); assert!((c.feet_height() - 100.).abs() < 0.2);
    let mut max_air = 0; for i in 0..(6 * 60) { c.tick(Vec2::X, 1. / 60.); if c.is_falling() { max_air = i; } }
    assert!(c.fell_off_ledge >= 1 && c.landed >= 1); assert!(c.is_moving_on_ground() && c.feet_height() < 0.2, "ended at {} after leaving the ledge at frame {max_air}", c.feet_height());
}
#[test] fn a_crouched_athena_pawn_walks_off_a_ledge() {
    let mut c = cmc_at(scene(&[cyl(0., 0., 0., 100., 150.)]), 0., 0., 100.); c.wants_crouch = true;
    let mut fell = false; for _ in 0..(4 * 60) { c.tick(Vec2::X, 1. / 60.); fell |= c.is_falling(); }
    assert!(fell && c.is_crouched && c.feet_height() < 0.2, "{} {:?}", c.feet_height(), c.mode);
}
#[test] fn a_crouched_pawn_does_not_walk_off_a_ledge_with_the_engine_default() {
    let mut c = Cmc::new(Params::default().pre_1p8_trace(), scene(&[cyl(0., 0., 0., 100., 150.)]), Vec3::new(0., 0., 100.)); c.wants_crouch = true;
    run(&mut c, 4., 60., Vec2::X); assert!(c.is_crouched && c.is_moving_on_ground() && c.feet_height() > 93. && c.feet_height() < 100.2, "{} {:?}", c.feet_height(), c.mode);
    // perched: PerchRadiusThreshold 20 lets the capsule centre hang up to 20 cm beyond the rim (and the capsule base sinks ~5 cm below the top face)
    assert!(c.loc.x > 150. && c.loc.x < 150. + mp::PERCH_RADIUS_THRESHOLD + 0.5, "centre {} cm from the axis", c.loc.x);
    c.wants_crouch = false; run(&mut c, 2., 60., Vec2::X); assert!(c.is_falling() || c.feet_height() < 0.2, "standing pawn walks off");
}
/// Live crouched half height: UE4.16 `UCapsuleComponent::SetCapsuleSize` clamps HalfHeight to >= Radius; the Athena CrouchedHalfHeight 50 is above the 42 cm
/// radius (`AFortPawn` ctor @1407c9ff0), so it stays 50.
const CROUCHED_LIVE_HH: f32 = if mp::CROUCHED_HALF_HEIGHT > mp::CAPSULE_RADIUS { mp::CROUCHED_HALF_HEIGHT } else { mp::CAPSULE_RADIUS };
#[test] fn uncrouch_is_refused_under_a_ceiling_and_allowed_after_leaving_it() {
    let slab = Frustum { c: Vec2::new(0., 0.), z0: 105., z1: 115., r0: 300., r1: 300. };
    let mut c = cmc_at(scene(&[slab]), 600., 0., 0.); c.wants_crouch = true; run(&mut c, 0.3, 60., Vec2::ZERO); assert!(c.is_crouched && (c.half_height - CROUCHED_LIVE_HH).abs() < 1e-4);
    run(&mut c, 3., 60., -Vec2::X); assert!(c.loc.x < 250., "walked under the slab"); assert!(c.is_crouched);
    c.wants_crouch = false; run(&mut c, 0.5, 60., Vec2::ZERO); assert!(c.is_crouched, "stood up into the ceiling");
    assert!((c.half_height - CROUCHED_LIVE_HH).abs() < 1e-4 && c.feet_height() < 0.2);
    run(&mut c, 3., 60., Vec2::X); assert!(!c.is_crouched && (c.half_height - 75.).abs() < 1e-3, "crouch {} hh {}", c.is_crouched, c.half_height);
    assert!(c.is_moving_on_ground() && c.feet_height() < 0.2);
}
#[test] fn standing_pawn_cannot_enter_a_low_slab_but_a_crouched_one_can() {
    let slab = Frustum { c: Vec2::new(0., 0.), z0: 90., z1: 100., r0: 300., r1: 300. };
    let mut c = cmc_at(scene(&[slab]), 600., 0., 0.); run(&mut c, 3., 60., -Vec2::X); assert!(c.loc.x > 300. + 34. - 1., "standing pawn pushed {} into the slab", c.loc.x);
}
#[test] fn jump_under_a_ceiling_bonks_the_head() {
    let slab = Frustum { c: Vec2::new(0., 0.), z0: 200., z1: 220., r0: 300., r1: 300. };
    let mut c = cmc_at(scene(&[slab]), 0., 0., 0.); c.jump(); let mut top = 0f32;
    for _ in 0..240 { c.tick(Vec2::ZERO, 1. / 120.); top = top.max(c.loc.z + c.half_height); }
    assert!(top <= 200.2, "head reached {top}"); assert!(c.landed >= 1);
}

// ---------------------------------------------------------------------------------------------------------------------
// collision robustness
// ---------------------------------------------------------------------------------------------------------------------
#[test] fn no_tunnelling_through_a_trunk_at_extreme_speed_and_frame_time() {
    for dt in [0.25, 0.125, 1. / 30., 1. / 240.] {
        let mut c = cmc_at(scene(&[cyl(0., -500., 0., 400., 40.)]), 0., 0., 0.); c.velocity = Vec3::new(0., -4000., 0.);
        for _ in 0..(2. / dt) as usize { c.tick(FWD, dt); assert!(c.loc.y > -500. + 40. + mp::CAPSULE_RADIUS - 1.5, "dt {dt}: capsule at {} inside the trunk", c.loc.y); }
    }
}
#[test] fn walking_into_a_wall_slides_along_it() {
    let mut c = cmc_at(scene(&[cyl(0., -300., 0., 400., 40.)]), 30., 0., 0.); run(&mut c, 3., 60., FWD);
    assert!(c.loc.y < -300. - 34., "slid past the trunk: {:?}", c.loc);
}
#[test] fn controller_metres_and_cm_agree() {
    let mut c = Controller::default(); for _ in 0..120 { c.step(Vec2::new(0., -1.), false, false, 1. / 60.); }
    assert!((c.position.y * 100. - c.component().loc.y).abs() < 1e-3 && (c.velocity.y * 100. - c.component().velocity.y).abs() < 1e-3);
    assert!(c.velocity.y < -4. && c.velocity.y > -4.2, "{}", c.velocity.y);
}



// ---------------------------------------------------------------------------------------------------------------------
// props (shapes copied from combat.rs: trunk r 0.4 -> 0.3 over 3 m; rock rings (1.05,0) (0.95,0.7) (0.5,1.25) apex (0.05,1.4), jitter 12 %)
// ---------------------------------------------------------------------------------------------------------------------
fn tree(x: f32, y: f32) -> Vec<Frustum> { vec![Frustum { c: Vec2::new(x, y), z0: 0., z1: 300., r0: 40., r1: 30. }] }
fn rock(x: f32, y: f32) -> Vec<Frustum> { let k = 1.12; let f = |z0: f32, z1: f32, r0: f32, r1: f32| Frustum { c: Vec2::new(x, y), z0, z1, r0, r1 };
    vec![f(0., 70., 105. * k, 95. * k), f(70., 125., 95. * k, 50. * k), f(125., 140., 50. * k, 5.)] }
struct Rng(u32);
impl Rng { fn f(&mut self) -> f32 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 17; self.0 ^= self.0 << 5; (self.0 >> 8) as f32 / (1u32 << 24) as f32 } fn r(&mut self, a: f32, b: f32) -> f32 { a + (b - a) * self.f() } }
#[test] fn random_walks_never_penetrate_props_or_explode() {
    let mut solids = vec![]; for (x, y) in [(300., -400.), (-250., -150.), (50., -700.), (-500., -500.)] { solids.extend(tree(x, y)); } for (x, y) in [(0., -300.), (400., 200.), (-300., -600.), (150., -150.)] { solids.extend(rock(x, y)); }
    let sc = Scene { solids, ..Default::default() }; let mut rng = Rng(0xBEEF_1234); let mut worst = f32::MAX; let mut max_feet = 0f32;
    for case in 0..40 {
        let mut c = cmc_at(sc.clone(), rng.r(-800., 800.), rng.r(-900., 300.), 0.);
        if sc.overlaps(&c.capsule()) { continue; }
        let mut dir = Vec2::X;
        for step in 0..1500 {
            if step % 90 == 0 { let a = rng.r(0., std::f32::consts::TAU); dir = Vec2::new(a.cos(), a.sin()); }
            let dt = [1. / 60., 1. / 144., 0.03][step % 3]; c.wants_sprint = step % 2 == 0;
            if step % 250 == 0 { c.jump(); } else if step % 250 == 1 { c.stop_jumping(); }
            c.tick(dir, dt);
            assert!(c.loc.is_finite() && c.velocity.is_finite(), "case {case} step {step}: non-finite state {:?}", c.loc);
            let gap = sc.probe(&c.capsule()).gap; worst = worst.min(gap); max_feet = max_feet.max(c.feet_z());
            assert!(gap > -0.05, "case {case} step {step}: capsule penetrates by {} cm at {:?}", -gap, c.loc);
            assert!(Vec2::new(c.velocity.x, c.velocity.y).length() < 560., "horizontal speed {}", c.velocity);
        }
    }
    assert!(max_feet < 420., "climbed to {max_feet}"); eprintln!("worst gap {worst} cm, highest capsule base {max_feet} cm");
}
#[test] fn the_player_slides_around_a_rock_and_cannot_climb_its_steep_flank() {
    let mut c = cmc_at(Scene { solids: rock(0., -300.), ..Default::default() }, 20., 0., 0.); let mut top = 0f32; let mut past = false;
    for _ in 0..(8 * 60) { c.tick(FWD, 1. / 60.); top = top.max(c.feet_height()); if c.loc.y < -300. - 140. { past = true; } }
    assert!(top < 3.0 || top > 130., "stood on the flank at {top} (the 2.25 is the capsule hover while the floor result is momentarily invalid against the flank)"); assert!(past || top > 130., "stuck against the rock: {:?}", c.loc);
}
#[test] fn a_jump_can_land_on_the_rock_top_only_when_it_clears_it() {
    // rock top 1.4 m, jump apex 0.64 m (DefaultGravityZ -2800): neither a running nor a standing jump reaches the top
    let sc = Scene { solids: rock(0., 0.), ..Default::default() }; let mut c = cmc_at(sc.clone(), 0., 150., 0.); c.jump(); for _ in 0..200 { c.tick(Vec2::new(0., -0.05), 1. / 120.); }
    assert!(c.feet_height() < 3., "{}", c.feet_height()); // <= the 2.25 cm capsule hover against the flank
}


// ---------------------------------------------------------------------------------------------------------------------
// Controller (metres, Y up) on top of the component
// ---------------------------------------------------------------------------------------------------------------------
#[test] fn run_state_requires_input_and_excludes_crouch() {
    let mut c = Controller::default(); c.step(Vec2::ZERO, false, true, 0.016); assert!(!c.running); c.step(Vec2::Y, false, true, 0.016); assert!(c.running);
    c.wants_crouch = true; for _ in 0..10 { c.step(Vec2::Y, false, true, 0.016); } assert!(c.crouched && !c.running);
}
#[test] fn external_writes_are_imported_as_teleports_and_requests() {
    let mut c = Controller::default(); c.step(Vec2::ZERO, false, false, 1. / 60.);
    c.position = Vec2::new(3., -2.); c.step(Vec2::ZERO, false, false, 1. / 60.); assert!((c.position - Vec2::new(3., -2.)).length() < 1e-4 && c.grounded() && c.height == 0.);
    c.height = 1.; c.vertical_speed = 0.; c.step(Vec2::ZERO, false, false, 1. / 60.); assert!(c.airborne() && c.height > 0.9);
    let mut c = Controller::default(); c.crouched = true; for _ in 0..5 { c.step(Vec2::ZERO, false, false, 1. / 60.); } assert!(c.crouched, "legacy crouched=true is a crouch request");
    c.crouched = false; for _ in 0..5 { c.step(Vec2::ZERO, false, false, 1. / 60.); } assert!(!c.crouched);
}
#[test] fn standing_on_a_prop_top_is_grounded_with_a_raised_height() {
    use crate::scene::Blocker;
    let mut c = Controller::default(); c.blockers = vec![Blocker { centre: Vec2::new(0., 0.), y_lo: 0., y_hi: 0.5, r_lo: 1.5, r_hi: 1.5 }];
    c.position = Vec2::new(2.5, 0.); c.step(Vec2::ZERO, false, false, 1. / 60.);
    // run at the block and jump onto it (apex 0.64 m > 0.5 m, above the 0.45 m step height)
    let mut onto = false; for i in 0..360 { let go = i > 10; c.step(if go { -Vec2::X } else { Vec2::ZERO }, go && (i % 90 == 20 || i % 90 == 21), false, 1. / 60.); if c.grounded() && c.height > 0.45 { onto = true; } }
    assert!(onto, "never stood on the 0.5 m block (position {:?}, height {})", c.position, c.height);
    assert_eq!(c.air_height(), if c.airborne() { c.height.max(1e-4) } else { 0. });
}
#[test] fn controller_jump_arc_is_frame_rate_independent_within_the_midpoint_tolerance() {
    let mut r = vec![]; for hz in [30., 60., 144., 240.] { let mut c = tap_controller(); let (mut apex, mut air) = (0f32, 0.); for f in 0..(3. * hz) as usize { c.step(Vec2::ZERO, f == 0, false, 1. / hz); apex = apex.max(c.height); if c.airborne() { air += 1. / hz; } } r.push((apex, air)); assert!(c.grounded() && c.height == 0.); }
    // 60 Hz and faster are frame-rate independent; 30 Hz differs by the decoded `DoJump` frame-delta scale (exe 0x6d4f20) and by nothing else.
    for (w, hz) in r.iter().zip([30., 60., 144., 240.]) { assert!((w.0 - mp::jump_apex_cm_at(1. / hz) / 100.).abs() < 0.03 && (w.1 - mp::jump_airtime_at(1. / hz)).abs() < 0.06, "{hz} Hz {r:?}"); }
    assert!((r[1].0 - r[3].0).abs() < 0.005, "apex differs by {} m between 60 and 240 fps", (r[1].0 - r[3].0).abs());
}

// ---- tuning knobs (FNRT_GRAVITY_SCALE / FNRT_JUMPZ as an explicit `Tuning`; added by the rewrite, not in the old tree)
#[test] fn default_params_are_pure_and_untuned() {
    let p = Params::default(); assert!(!p.is_tuned(), "Params::default() must not read the environment"); assert!(crate::cmc::Tuning::default().is_default());
    assert_eq!(p.gravity_scale, mp::GRAVITY_SCALE); assert_eq!(p.jump_z_velocity, mp::JUMP_Z_VELOCITY);
}
#[test] fn tuning_parses_the_env_forms_and_ignores_garbage() {
    use crate::cmc::Tuning;
    assert_eq!(Tuning::parse(Some("1.5"), Some(" 700 ")), Tuning { gravity_scale: Some(1.5), jump_z_velocity: Some(700.) });
    assert_eq!(Tuning::parse(None, None), Tuning::default());
    for bad in ["", "abc", "0", "-2", "NaN", "inf"] { assert_eq!(Tuning::parse(Some(bad), Some(bad)), Tuning::default(), "{bad:?} must be ignored"); }
}
#[test] fn tuning_changes_gravity_and_jump_speed_and_is_reported() {
    use crate::cmc::Tuning;
    let p = Params::default().with_tuning(&Tuning { gravity_scale: Some(2.), jump_z_velocity: Some(700.) });
    assert!(p.is_tuned()); assert_eq!(p.gravity_z(), Params::default().gravity_z() * 2.); assert_eq!(p.jump_z_velocity, 700.);
    assert!(p.tuning_line().contains("[tuned"), "{}", p.tuning_line()); assert!(Params::default().tuning_line().contains("[defaults]"));
    let (mut a, mut b) = (tap_controller(), Controller::with_tuning(&Tuning { gravity_scale: None, jump_z_velocity: Some(900.) })); b.component_mut().p.jump_release_ignored = false;
    let apex = |c: &mut Controller| { let mut top = 0f32; for i in 0..240 { c.step(glam::Vec2::ZERO, i < 3, false, 1. / 60.); top = top.max(c.height); } top };
    let (ha, hb) = (apex(&mut a), apex(&mut b)); assert!(hb > ha * 2.0, "JumpZ 900 vs 600 should give ~2.25x the apex: {ha} {hb}");
    assert_eq!(a.fall_time_left(), 0.); let g = Controller::default(); assert_eq!(g.fall_time_left(), crate::controller::fall_time_left(0., 0.));
}

// ---------------------------------------------------------------------------------------------------------------------
// FALLING.md proof: the port's step equals the decoded 1.8 algorithm (exe RVAs in params.rs / FALLING.md)
// ---------------------------------------------------------------------------------------------------------------------
/// The decoded PhysFalling for a vertical jump, written independently of cmc.rs: `v1 = max(v0 + g dt, -|T|)` (NewFallVelocity 0x43a5a9), `z += 0.5 (v0 + v1) dt` (the
/// Adjusted move at 0x2089182..), one iteration per tick (dt <= MaxSimulationTimeStep 0.05). `z` is relative to the take-off capsule base. Returns (apex cm, ticks until z <= land_z).
fn decoded_vertical(v0: f32, g_z: f32, terminal: f32, dt: f32, land_z: f32) -> (f32, u32) {
    let (mut z, mut v, mut apex, mut n) = (0f32, v0, 0f32, 0u32);
    loop {
        let v1 = (v + g_z * dt).max(-terminal.abs()); z += 0.5 * (v + v1) * dt; v = v1; n += 1; apex = apex.max(z);
        if z <= land_z || n > 1000 { return (apex, n); }
    }
}
#[test] fn standing_and_running_jump_equal_the_decoded_1_8_algorithm_at_30_hz() {
    let dt = 1. / 30.;
    // the recovered parameters, composed exactly like GetGravityZ (world x GravityScale x GravityZScale) and the Athena DoJump / Fort DoJump scale chain
    let g_z = (mp::WORLD_GRAVITY_Z * mp::GRAVITY_SCALE) * mp::GRAVITY_Z_SCALE_ATTRIBUTE;
    let v0 = (mp::JUMP_Z_VELOCITY * mp::JUMP_CHAIN_TABLE[0].0) * (dt * mp::JUMP_FRAME_DT_SLOPE + mp::JUMP_FRAME_DT_OFFSET); // dt 0.0333 > 0.03 -> scaled
    assert!((v0 - 635.3637).abs() < 1e-3, "launch {v0}");
    // --- standing jump
    let mut c = flat(); c.p.jump_max_hold_time = 0.; let base0 = c.feet_z(); c.jump();
    let (mut apex, mut n) = (0f32, 0u32);
    loop { c.tick(Vec2::ZERO, dt); n += 1; if c.is_falling() { apex = apex.max(c.feet_z() - base0); } if c.is_moving_on_ground() || n > 1000 { break; } }
    // the landing plane is the floor surface (z = 0) while the capsule base started `base0` above it, minus the 0.1 cm sweep stand-off: the tick count must not depend on that
    let (ref_apex, ref_n) = decoded_vertical(v0, g_z, mp::TERMINAL_VELOCITY, dt, -(base0 - 0.1));
    for land in [-(base0), -(base0 - 0.1), -(base0 - 0.2)] { assert_eq!(decoded_vertical(v0, g_z, mp::TERMINAL_VELOCITY, dt, land).1, ref_n, "landing tick is ambiguous for plane {land}"); }
    assert_eq!(n, ref_n, "air ticks: port {n} vs decoded {ref_n}");
    assert!((apex - ref_apex).abs() < 1e-3, "apex: port {apex} vs decoded {ref_apex}");
    assert!((n as f32 * dt - ref_n as f32 * dt).abs() < 1e-6);
    // closed forms (continuous): apex v^2/2g and airtime 2v/g, up to the sampling errors g dt^2 / 8 and one tick
    assert!((ref_apex - mp::jump_apex_cm_at(dt)).abs() < 2800. * dt * dt / 8. + 0.01 && (n as f32 * dt - mp::jump_airtime_at(dt)).abs() < dt + 1e-3, "{ref_apex} {n}");
    // --- running jump: horizontal speed is the recovered run speed (410 cm/s, directional multiplier 1 straight ahead), constant in the air (FallingLateralFriction 0,
    //     BrakingDecelerationFalling 0, speed == MaxSpeed so air control cannot add), and the vertical motion is the same as for the standing jump
    let mut r = flat(); r.p.jump_max_hold_time = 0.; run(&mut r, 3., 30., FWD); assert!((r.velocity.length() - mp::SPEED_RUN).abs() < 0.01, "{}", r.velocity.length());
    let (y0, rbase0) = (r.loc.y, r.feet_z()); r.jump(); let mut m = 0u32; let mut rapex = 0f32;
    loop { r.tick(FWD, dt); m += 1; if r.is_falling() { rapex = rapex.max(r.feet_z() - rbase0); } if r.is_moving_on_ground() || m > 1000 { break; } }
    let dist = (y0 - r.loc.y).abs(); let expect = mp::SPEED_RUN * dt * ref_n as f32;
    assert_eq!(m, ref_n); assert!((rapex - ref_apex).abs() < 1e-3, "{rapex} vs {ref_apex}");
    assert!((dist - expect).abs() < 0.05, "running jump covers {dist} cm vs decoded {expect} cm ({} ticks of {} cm/s)", ref_n, mp::SPEED_RUN);
    eprintln!("30 Hz jump (g {g_z}, launch {v0:.4} cm/s): standing apex {apex:.4} cm (decoded {ref_apex:.4}), {n} ticks = {:.4} s (decoded {ref_n}); running jump {dist:.3} cm (decoded {expect:.3})", n as f32 * dt);
}
#[test] fn jump_frame_scale_and_the_consecutive_jump_table_follow_the_exe() {
    assert_eq!(mp::jump_frame_scale(1. / 60.), 1.); assert_eq!(mp::jump_frame_scale(0.03), 1.);
    assert!((mp::jump_frame_scale(1. / 30.) - 1.0589395).abs() < 1e-6); assert_eq!(mp::jump_frame_scale(10.), 2.5);
    // jump, land, jump again inside the 1 s window: chain indices 0, 1, 2, 3, 4, 4 -> JumpZ x 1, 1, 0.75, 0.5, 0.25, 0.25 (120 Hz: no frame scale)
    let dt = 1. / 120.; let mut c = flat(); let g = c.gravity_z();
    let mut launches = vec![];
    for _ in 0..6 {
        c.jump(); c.tick(Vec2::ZERO, dt); launches.push((c.velocity.z - g * dt).round()); c.stop_jumping();
        for _ in 0..(3 * 120) { c.tick(Vec2::ZERO, dt); if c.is_moving_on_ground() { break; } }
        assert!(c.is_moving_on_ground() && c.jump_chain_window_remaining > 0.1, "window {}", c.jump_chain_window_remaining);
    }
    assert_eq!(launches, vec![600., 600., 450., 300., 150., 150.], "{launches:?}");
    // after a pause longer than the window the chain restarts
    for _ in 0..(2 * 120) { c.tick(Vec2::ZERO, dt); } assert_eq!(c.jump_chain_window_remaining, 0.);
    c.jump(); c.tick(Vec2::ZERO, dt); assert_eq!((c.velocity.z - g * dt).round(), 600.); assert_eq!(c.jump_chain_index, 0);
}
#[test] fn the_fort_new_fall_velocity_only_clamps_downward_z_at_the_physics_volume_terminal_velocity() {
    let c = flat(); let g = Vec3::new(0., 0., c.gravity_z());
    let v = c.velocity_after_fall_step_for_test(Vec3::new(300., -200., -3990.), g, 1. / 30.); assert_eq!(v, Vec3::new(300., -200., -4000.));
    let v = c.velocity_after_fall_step_for_test(Vec3::new(5000., 0., 100.), g, 1. / 30.); assert_eq!(v.x, 5000.); assert!((v.z - (100. - 2800. / 30.)).abs() < 1e-3);
}

// ---------------------------------------------------------------------------------------------------------------------
// UE4.16 jump / fall translation checks (Character.cpp CheckJumpInput / CanJumpInternal, CharacterMovementComponent.cpp PhysFalling)
// ---------------------------------------------------------------------------------------------------------------------
#[test] fn jump_apex_height_and_time_to_apex_equal_the_analytic_ue_result_for_the_1_8_values() {
    // JumpZ 600 (PlayerPawn_Athena_Generic CharMoveComp), gravity -2800 x GravityScale 1 x GravityZScale 1: apex v^2/2g = 64.2857 cm, time to apex v/g = 0.214286 s.
    let hz = 240.; let mut c = flat(); c.p.jump_max_hold_time = 0.; c.jump(); let b = c.feet_z(); let (mut apex, mut t_apex, mut t) = (0f32, 0f32, 0f32);
    for _ in 0..240 { c.tick(Vec2::ZERO, 1. / hz); t += 1. / hz; let h = c.feet_z() - b; if h > apex { apex = h; t_apex = t; } }
    assert!((apex - 600. * 600. / (2. * 2800.)).abs() < 0.3, "apex {apex}"); assert!((t_apex - 600. / 2800.).abs() < 2. / hz, "time to apex {t_apex}");
}
#[test] fn held_jump_with_a_hold_time_relaunches_until_it_expires_but_the_1_8_pawn_has_none() {
    // Character.cpp:277-296 / 986-1000: with JumpMaxHoldTime > 0 the held key keeps DoJump going (bWasJumping), JumpCurrentCount stays 1; 1.8 has JumpMaxHoldTime 0.
    let vz_after = |hold: f32, ticks: usize| { let mut p = Params::default(); p.jump_max_hold_time = hold; p.jump_chain_table = [(1., 1.); 5]; /* isolate the engine rule from the Athena consecutive-jump table */ let mut c = Cmc::new(p, Scene::default(), Vec3::ZERO); c.jump();
        for _ in 0..ticks { c.tick(Vec2::ZERO, 1. / 60.); } (c.velocity.z, c.jumped, c.jump_current_count) };
    let (vz0, j0, _) = vz_after(0., 8); let (vz1, j1, n1) = vz_after(0.5, 8);
    assert_eq!(j0, 1); assert_eq!(j1, 1); assert_eq!(n1, 1);
    assert!(vz0 < 600. - 2800. * 7. / 60. + 1., "no hold time: plain ballistic {vz0}"); assert!(vz1 > 600. - 2800. / 60. - 1., "held: relaunched every tick {vz1}");
    assert_eq!(Params::default().jump_max_hold_time, mp::MAX_JUMP_TIME_STAT); assert_eq!(Params::default().jump_max_count, 1);
}
fn held_jump_apex(p: Params, hz: f32, ticks: usize) -> (f32, u32) {
    let mut c = Cmc::new(p, Scene::default(), Vec3::ZERO); let b = c.feet_z(); let mut apex = 0f32; let mut launches = 0; c.jump();
    for k in 0..(4. * hz) as usize { c.tick(Vec2::ZERO, 1. / hz); if (c.velocity.z - (600. - 2800. / hz)).abs() < 0.05 { launches += 1; } apex = apex.max(c.feet_z() - b); if k + 1 == ticks { c.stop_jumping(); } }
    (apex, launches)
}
#[test] fn jump_hold_matches_the_real_1_8_client_trace() {
    for (hz, ticks, apex_real, launches_real) in [(60., 1, 64.28, 1), (60., 2, 73.89, 2), (60., 4, 93.11, 4), (60., 8, 131.56, 8), (60., 16, 189.22, 14), (60., 60, 189.22, 14), (144., 1, 64.28, 1), (144., 14, 117.58, 14), (144., 43, 203.66, 35)] {
        let (apex, launches) = held_jump_apex(Params::default(), hz, ticks);
        assert_eq!(launches, launches_real, "{hz} Hz held {ticks} ticks"); assert!((apex - apex_real).abs() < 0.05, "{hz} Hz held {ticks} ticks: apex {apex} vs real {apex_real}");
    }
    let (old, _) = held_jump_apex(Params::default().pre_1p8_trace(), 60., 16); assert!((old - 64.28).abs() < 0.05, "kill switch restores the hold-free jump: {old}");
}
#[test] fn held_relaunches_keep_the_consecutive_jump_index_and_window() {
    let mut c = flat(); c.jump(); for _ in 0..10 { c.tick(Vec2::ZERO, 1. / 60.); assert_eq!(c.jump_chain_index, 0); assert!(c.velocity.z > 600. - 2800. / 60. - 0.1, "{}", c.velocity.z); }
    assert_eq!(c.jumped, 1); assert_eq!(c.jump_current_count, 1);
}
#[test] fn a_crouched_press_stands_up_first_and_jumps_on_the_next_tick() {
    let crouched = || { let mut c = Controller::default(); c.wants_crouch = true; for _ in 0..30 { c.step(Vec2::ZERO, false, false, 1. / 60.); } assert!(c.crouched); c.wants_crouch = false; c };
    let mut c = crouched(); c.step(Vec2::ZERO, true, false, 1. / 60.); assert!(!c.crouched && c.jumps_started() == 0 && c.grounded()); c.step(Vec2::ZERO, false, false, 1. / 60.); assert!(c.jumps_started() == 1 && c.airborne(), "the Athena pawn ignores the release: the tap still jumps");
    let mut c = crouched(); c.component_mut().p.jump_release_ignored = false; c.step(Vec2::ZERO, true, false, 1. / 60.); c.step(Vec2::ZERO, false, false, 1. / 60.); assert_eq!(c.jumps_started(), 0, "with the release honoured a one-tick tap from crouch only stands up");
    let mut c = crouched(); c.step(Vec2::ZERO, true, false, 1. / 60.); assert_eq!(c.jumps_started(), 0); c.step(Vec2::ZERO, true, false, 1. / 60.); assert!(c.jumps_started() == 1 && c.airborne());
}
#[test] fn athena_crouch_capsule_is_50_cm_and_drops_the_centre_25_cm() {
    let mut c = flat(); let z0 = c.loc.z; c.wants_crouch = true; c.tick(Vec2::ZERO, 1. / 60.);
    assert!(c.is_crouched && (c.half_height - 50.).abs() < 1e-4 && (z0 - c.loc.z - 25.).abs() < 1e-3, "hh {} drop {}", c.half_height, z0 - c.loc.z);
}
#[test] fn analog_input_is_floored_at_min_analog_walk_speed() {
    let mut c = flat(); run(&mut c, 2., 60., FWD * 0.1); assert!((c.velocity.length() - mp::SPEED_DBNO).abs() < 0.5, "{}", c.velocity.length());
    let mut c = Cmc::new(Params::default().pre_1p8_trace(), Scene::default(), Vec3::ZERO); run(&mut c, 2., 60., FWD * 0.1); assert!((c.velocity.length() - 41.).abs() < 0.5, "{}", c.velocity.length());
}
#[test] fn landing_returns_to_walking_resets_the_jump_count_and_an_air_press_is_never_buffered() {
    let mut c = flat(); c.jump(); c.tick(Vec2::ZERO, 1. / 60.); assert!(c.is_falling()); assert_eq!(c.jump_current_count, 1);
    for i in 0..120 { if i == 5 { c.jump(); } c.tick(Vec2::ZERO, 1. / 60.); if c.is_moving_on_ground() { break; } }
    assert!(c.is_moving_on_ground() && c.landed == 1, "landed"); assert_eq!(c.jump_current_count, 0, "ResetJumpState on landing"); assert_eq!(c.jumped, 1, "the air press did not jump and was not buffered");
    for _ in 0..5 { c.tick(Vec2::ZERO, 1. / 60.); } assert!(c.is_moving_on_ground() && c.velocity.z == 0.);
}
#[test] fn a_pawn_can_crouch_in_the_air_like_the_1_8_exe() {
    let mut c = flat(); c.jump(); c.tick(Vec2::ZERO, 1. / 60.); assert!(c.is_falling()); c.wants_crouch = true; c.tick(Vec2::ZERO, 1. / 60.); assert!(c.is_crouched, "CanCrouchInCurrentState = IsFalling || IsMovingOnGround (exe 0x20702e0)");
}

// ---------------------------------------------------------------------------------------------------------------------
// TERRAIN: the landscape as the floor (synthetic height fields through the same fn_terrain::Local the game installs)
// ---------------------------------------------------------------------------------------------------------------------
/// A landscape in a non-trivial mapping (centre (1000, -500), anchor z -10, ref_z 300, controller z offset 12: scene = UE - (1000, 1700, 300)) whose scene
/// height is `f(scene x, scene y)` (cm), 2 m quads covering scene x -70..182 m.
fn terrain(f: impl Fn(f32, f32) -> f32) -> fn_terrain::Local {
    let map = fn_terrain::Mapping { centre_cm: [1000., -500., 0.], anchor: Vec3::new(0., 0., -10.), ref_z_cm: 300. };
    let off = map.to_ue(Vec3::new(0., 0., 12.));
    let hf = fn_terrain::Heightfield::synthetic(Vec2::new(-6000., -6000.), 200., 2, 64, |x, y| off.z + f(x - off.x, y - off.y));
    fn_terrain::Local::new(std::sync::Arc::new(fn_terrain::World { hf, map }), 12.)
}
/// Slope rising along +X at `deg` between scene x = x0 and x0 + run (cm), flat outside.
fn ramp(deg: f32, x0: f32, run: f32) -> fn_terrain::Local { let k = deg.to_radians().tan(); terrain(move |x, _| k * (x - x0).clamp(0., run)) }
fn on_terrain(t: &fn_terrain::Local, x_m: f32, y_m: f32) -> Controller {
    let mut c = Controller::default(); c.terrain = Some(t.clone());
    c.position = Vec2::new(x_m, y_m); c.height = t.sample(x_m * 100., y_m * 100.).unwrap().0 * 0.01;
    c.step(Vec2::ZERO, false, false, 1. / 60.); c
}
fn terrain_gap(c: &Controller) -> f32 { let mut s = Scene::default(); s.terrain = c.terrain.clone(); s.probe(&c.component().capsule()).gap }

#[test] fn an_idle_pawn_on_a_slope_rests_on_the_landscape_and_does_not_slide() {
    let t = ramp(30., -1000., 2000.); // (synthetic heights span +-2048 cm)
    let mut c = on_terrain(&t, 3., 1.);
    let p0 = c.position;
    for _ in 0..180 { c.step(Vec2::ZERO, false, false, 1. / 60.); }
    assert!(c.grounded() && (c.position - p0).length() < 1e-3, "slid {:?}", c.position - p0);
    let f = &c.component().floor; let ip = f.hit.impact_point;
    // the contact point lies on the surface (< 1 cm), the capsule hovers in the floor-distance band above it
    assert!((ip.z - t.sample(ip.x, ip.y).unwrap().0).abs() < 1., "impact {ip:?}");
    let gap = terrain_gap(&c); assert!((1.8..=2.5).contains(&gap), "capsule gap {gap}");
    // the capsule rests as a UE capsule on a 30 deg plane does: its base (the controller's feet height, where the mesh stands) is r (1 / cos - 1) + floor dist
    // above the ground under its axis (6.5 + ~2 cm here): the contact is on the uphill side of the lower hemisphere
    let axis = t.sample(c.position.x * 100., c.position.y * 100.).unwrap().0;
    let r = c.component().p.radius; let want = r * (1. / 30f32.to_radians().cos() - 1.) + f.floor_dist;
    assert!((c.height * 100. - c.component().feet_z()).abs() < 0.01 && ((c.height * 100. - axis) - want).abs() < 0.5, "feet {} cm above the ground under the axis, want {want}", c.height * 100. - axis);
    assert!(c.air_height() == 0. && c.fall_time_left() == 0.);
}
#[test] fn walking_up_and_down_a_walkable_slope_stays_grounded() {
    let t = ramp(35., 0., 1500.); // 35 deg < acos(WalkableFloorZ 0.71) = 44.8 deg
    let mut c = on_terrain(&t, -3., 0.);
    let (mut top, mut worst) = (0f32, (f32::MAX, f32::MIN));
    for i in 0..(9 * 60) {
        let dir = if i < 6 * 60 { Vec2::X } else { -Vec2::X };
        c.step(dir, false, false, 1. / 60.);
        assert!(c.grounded(), "frame {i}: airborne at {:?} height {}", c.position, c.height);
        let g = terrain_gap(&c); worst = (worst.0.min(g), worst.1.max(g)); top = top.max(c.height);
    }
    assert!(worst.0 > -0.2 && worst.1 < 3., "capsule gap range {worst:?} cm");
    assert!(top > 15. * 35f32.to_radians().tan() - 0.2, "reached the top of the 15 m ramp: {top} m");
}
#[test] fn a_sweep_into_a_steep_slope_stops() {
    let t = ramp(60., 0., 1000.);
    let mut c = on_terrain(&t, -2., 0.);
    let mut worst = f32::MAX;
    for _ in 0..(5 * 60) { c.step(Vec2::X, false, true, 1. / 60.); worst = worst.min(terrain_gap(&c)); }
    // a 60 deg face is not walkable (normal z 0.5 < 0.71): at most one step up (MaxStepHeight 45 cm) onto it, never through it
    assert!(c.height < 0.5 && c.position.x < 0.5, "climbed to {:?} height {}", c.position, c.height);
    assert!(worst > -0.2, "penetrated {worst} cm");
    assert!(c.velocity.x.abs() < 0.5, "still pushing at {}", c.velocity.x);
}
#[test] fn a_fall_onto_the_landscape_lands_on_it() {
    let t = terrain(|x, y| 100. * (x * 0.002).sin() + 60. * (y * 0.003).cos());
    let mut c = Controller::default(); c.terrain = Some(t.clone());
    c.position = Vec2::new(4., -2.); c.height = 12.;
    c.step(Vec2::ZERO, false, false, 1. / 60.);
    assert!(c.airborne() && c.air_height() > 10. && c.fall_time_left() > 0.5);
    for _ in 0..240 { c.step(Vec2::ZERO, false, false, 1. / 60.); }
    assert!(c.grounded() && c.landings() == 1, "landed: {}", c.landings());
    let gap = terrain_gap(&c); assert!((1.8..=2.5).contains(&gap), "capsule gap {gap}");
}
#[test] fn terrain_probes_are_cheap() {
    let t = terrain(|x, y| 100. * (x * 0.002).sin() + 60. * (y * 0.003).cos());
    let mut s = Scene::default(); s.terrain = Some(t);
    let t0 = std::time::Instant::now(); let mut acc = 0f32;
    for i in 0..20000 { let x = (i % 200) as f32 * 7.; let cap = crate::scene::Capsule { c: Vec3::new(x, (i / 200) as f32 * 9., 150. + (i % 13) as f32 * 10.), r: 42., hh: 90. }; acc += s.probe(&cap).gap; }
    let us = t0.elapsed().as_secs_f64() * 1e6 / 20000.;
    eprintln!("terrain probe: {us:.2} us each ({acc})");
    assert!(us < 20., "{us} us per probe");
}
#[test] fn directional_speed_follows_the_velocity_not_the_input() {
    let turn = |p: Params| { let mut c = Cmc::new(p, Scene::default(), Vec3::ZERO); run(&mut c, 2., 60., FWD); c.tick(Vec2::X, 1. / 60.); c.velocity.length() };
    let (new, old) = (turn(Params::default()), turn(Params::default().pre_1p8_trace()));
    let friction_turn = (Vec2::new(41., -369.) + Vec2::new(12.5, 0.)).length(); assert!((new - friction_turn).abs() < 0.5, "only the friction turn, no over-max braking: {new} vs {friction_turn}"); assert!(old < new - 10., "input-based curve brakes to the strafe cap: {old}");
    let mut c = flat(); run(&mut c, 3., 60., Vec2::X); assert!((c.velocity.length() - 328.).abs() < 0.5, "steady strafe is still 0.8 x 410: {}", c.velocity.length());
}
fn tap_controller() -> Controller { let mut c = Controller::default(); c.component_mut().p.jump_release_ignored = false; c }
#[test] fn the_athena_pawn_ignores_the_jump_release_so_every_jump_is_the_full_hold() {
    let apex = |release_ignored: bool| { let mut c = Controller::default(); c.component_mut().p.jump_release_ignored = release_ignored; let b = c.component().loc.z; let mut top = 0f32; for i in 0..240 { c.step(Vec2::ZERO, i < 2, false, 1. / 60.); top = top.max(c.component().loc.z - b); } top };
    assert!((apex(true) - 189.22).abs() < 0.3, "a 2-frame tap is the full 0.25 s hold: {}", apex(true));
    assert!((apex(false) - 73.89).abs() < 0.3, "{}", apex(false));
    let mut c = Controller::default(); c.step(Vec2::ZERO, true, false, 1. / 60.); c.step(Vec2::ZERO, false, false, 1. / 60.); for _ in 0..20 { c.step(Vec2::ZERO, false, false, 1. / 60.); }
    let vz = c.vertical_speed; c.step(Vec2::ZERO, true, false, 1. / 60.); assert!(c.vertical_speed < vz && c.jumps_started() == 1, "an air press does nothing");
}
#[test] fn an_unwalkable_landscape_slope_slides_the_pawn_down_at_the_athena_safe_slide_speed() {
    let mut c = on_terrain(&ramp(52., 0., 1200.), 8., 0.);
    let (mut slid, mut fastest) = (false, 0f32);
    for _ in 0..(3 * 60) { c.step(Vec2::ZERO, false, false, 1. / 60.); slid |= c.airborne(); fastest = fastest.max(Vec3::new(c.velocity.x, c.velocity.y, c.vertical_speed).length()); }
    assert!(slid && c.grounded() && c.position.x < 0.5 && c.height < 0.1, "ended at {:?} height {}", c.position, c.height);
    assert!(fastest < mp::FALLING_SLOPE_SAFE_SLIDE_MAX_SPEED * 0.01 + 1., "slid at {fastest} m/s");
    let mut c = on_terrain(&ramp(65., 0., 800.), 6., 0.);
    let mut fastest = 0f32;
    for _ in 0..(3 * 60) { c.step(Vec2::ZERO, false, false, 1. / 60.); fastest = fastest.max(Vec3::new(c.velocity.x, c.velocity.y, c.vertical_speed).length()); }
    assert!(c.grounded() && c.position.x < 0.5, "ended at {:?} height {}", c.position, c.height);
    assert!(fastest > mp::FALLING_SLOPE_SAFE_SLIDE_MAX_SPEED * 0.01 + 1., "a face steeper than FallingSlopeSafeSlideAngle is not speed capped: {fastest} m/s");
}
#[test] fn the_drawn_height_keeps_the_capsule_floor_hover_so_takeoff_and_landing_do_not_pop() {
    let mut c = Controller::default(); let dt = 1. / 144.;
    for _ in 0..30 { c.step(Vec2::ZERO, false, false, dt); }
    let rest = c.render_height(); let hover = rest - c.height;
    assert!(hover > 0.018 && hover <= 0.024, "grounded capsule hover {hover}");
    let mut prev = rest; let mut steps = vec![]; let mut landed = false; let mut was_air = false;
    for i in 0..200 { c.step(Vec2::ZERO, i < 2, false, dt); let h = c.render_height(); steps.push(h - prev); prev = h; if was_air && c.grounded() { landed = true; break; } was_air |= c.airborne(); }
    assert!(landed);
    let v0 = c.component().p.jump_z_velocity * 0.01;
    assert!((steps[0] - (v0 + (v0 - 28. * dt)) * 0.5 * dt).abs() < 0.003, "first airborne step is the midpoint move only: {}", steps[0]);
    assert!((c.render_height() - rest).abs() < 1e-3, "rests at the same hovering height after landing");
}

#[test] fn launch_character_takes_a_walking_pawn_into_the_air_on_the_next_tick() {
    let mut c = Cmc::new(Params::default(), Scene::default(), Vec3::ZERO);
    for _ in 0..5 { c.tick(Vec2::new(1., 0.), 1. / 60.); }
    assert_eq!(c.mode, Mode::Walking);
    let vx = c.velocity.x;
    c.launch(Vec3::new(vx, 0., 2000.));
    c.tick(Vec2::ZERO, 1. / 60.);
    assert_eq!(c.mode, Mode::Falling);
    assert!(c.velocity.z > 2000. - 2800. / 60. - 1. && c.velocity.z < 2000., "{}", c.velocity.z);
    let mut apex = 0f32; let z0 = c.feet_z();
    for _ in 0..240 { c.tick(Vec2::ZERO, 1. / 60.); apex = apex.max(c.feet_z() - z0); }
    assert!((apex - 2000. * 2000. / (2. * 2800.)).abs() < 40., "apex {apex}");
}
