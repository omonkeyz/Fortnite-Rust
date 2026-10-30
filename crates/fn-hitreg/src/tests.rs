use super::*;
use crate::data::Presentation;
use crate::shoot::{select_aim, trace_world};
use crate::spread::{fmod, vrand_cone, Curve};
use fn_combat::{harvest_fx::FxEvent, Harvestable, Kind, World};
use fn_pose::ClipSourceDir;
use glam::Vec3;

// ---------------------------------------------------------------- synthetic rows (values copied from the 1.8 `AthenaRangedWeapons` rows, so the maths tests run without the install)

/// `Assault_Auto_Athena_C_Ore_T03` (common assault rifle).
fn ar_row() -> StatRow {
    StatRow {
        spread: 0.15, spread_downsights: 0.7, standing_still_spread_mult: 0.55, crouching_spread_mult: 0.8, jumping_falling_spread_mult: 1.25, sprinting_spread_mult: 1.5,
        min_speed_for_spread_mult: 90., max_speed_for_spread_mult: 100.,
        heat: [(2.85, 8.75), (4.1, 8.75), (5.5, 10.)], cool: [(2.85, 2.7), (4.1, 3.), (5.5, 3.2)],
        bullets_per_cartridge: 1, firing_rate: 5.5, burst_firing_rate: 5., cartridge_per_fire: 1, base_level: 1,
        dmg: [32., 22.848, 22.848, 22.848], env_dmg: [35., 24.99, 24.99, 24.99], rng: [2500., 3750., 25000., 25000.], max_actual_range: 9056.,
        zone_light: 1., zone_normal: 1., zone_critical: 2.5, zone_vulnerability: 0., auto_reload_delay_override: -1., clip_size: 30, reload_time: 2.3, ammo_cost_per_fire: 1,
        ..Default::default()
    }
}
/// `Shotgun_Standard_Athena_C_Ore_T03` (pump shotgun), 10 pellets, 10 cm sweep.
fn shotgun_row() -> StatRow {
    StatRow {
        spread: 0.9091, spread_downsights: 0.8, standing_still_spread_mult: 0.92, crouching_spread_mult: 0.85, jumping_falling_spread_mult: 1.05, sprinting_spread_mult: 1.1,
        min_speed_for_spread_mult: 90., max_speed_for_spread_mult: 100.,
        heat: [(3.96, 4.675), (4.675, 4.675), (5.28, 4.675)], cool: [(3.96, 7.0125), (4.675, 7.0125), (5.28, 7.0125)],
        bullets_per_cartridge: 10, firing_rate: 0.7, burst_firing_rate: 5., cartridge_per_fire: 1, base_level: 1,
        dmg: [9., 6.426, 6.426, 6.426], env_dmg: [10., 7.14, 7.14, 7.14], rng: [1250., 1500., 1500., 1500.], sweep_radius: 10.,
        zone_normal: 1., zone_critical: 2.5, auto_reload_delay_override: -1.,
        ..Default::default()
    }
}
fn gun(stats: StatRow) -> GunHit {
    GunHit { wid: "test".into(), row: "test".into(), stats, pres: Presentation::default(), ability: Default::default(), projectile: None, perfect_aim_when_targeting_min_spread: true, trigger: String::new(), ammo: String::new(), ..Default::default() }
}
fn wor(props: Vec<Harvestable>) -> World { World { props, ..Default::default() } }
/// Spread state of a weapon that has been held for 10 s with the given pawn state (all smoothed factors converged).
fn settled(s: &StatRow, inp: SpreadInputs) -> SpreadState {
    let mut st = SpreadState::default();
    for k in 0..600 { st.tick(s, &SpreadInputs { now: k as f32 / 60., ..inp }, 1. / 60., 1); }
    st
}
fn angle_between(a: Vec3, b: Vec3) -> f32 { a.dot(b).clamp(-1., 1.).acos() }
fn ref_y(p: &[(f32, f32); 3], x: f32) -> f32 {
    if x <= p[0].0 { return p[0].1; }
    for w in 0..2 { if x < p[w + 1].0 { let (a, b) = (p[w], p[w + 1]); return a.1 + (x - a.0) / (b.0 - a.0) * (b.1 - a.1); } }
    p[2].1
}

// ---------------------------------------------------------------- curves, random stream, cone

#[test]
fn rich_curve_add_key_and_eval_match_ue() {
    let mut c = Curve::default();
    c.add_key(2.85, 8.75); c.add_key(4.1, 8.75); c.add_key(5.5, 10.);
    assert_eq!(c.time_range(), (2.85, 5.5));
    assert_eq!(c.eval(0., 0.), 8.75, "before the first key: first value");
    assert_eq!(c.eval(2.85, 0.), 8.75);
    assert!((c.eval(4.8, 0.) - (8.75 + (4.8 - 4.1) / 1.4 * 1.25)).abs() < 1e-5, "linear between keys");
    assert_eq!(c.eval(9., 0.), 10., "after the last key: last value");
    assert_eq!(Curve::default().eval(1., 7.), 7., "no keys: the default value");
    // equal times: AddKey inserts before the first key whose Time >= the new time; Eval returns the first key at or before its time, Diff == 0 returns the previous value
    let mut d = Curve::default(); d.add_key(10., 5.); d.add_key(10., 6.); d.add_key(20., 7.);
    assert_eq!(d.keys, vec![(10., 6.), (10., 5.), (20., 7.)]);
    assert_eq!(d.eval(10., 0.), 6., "t <= first key time -> first key");
    assert!((d.eval(15., 0.) - 6.).abs() < 1e-6, "between the second 10-key (5) and the 20-key (7)");
}

#[test]
fn random_stream_is_ue_frandomstream() {
    // seed 12345: Seed' = Seed * 196314165 + 907633515 (u32 wrap) = 2044445496; fraction = float(0x3f800000 | (Seed' & 0x7fffff)) - 1. The numbers come from an independent
    // 64-bit integer + BitConverter computation of the UE4.16 FRandomStream::GetFraction formula.
    let mut rs = RandomStream::new(12345);
    assert!((rs.fraction() - 0.716_895_1).abs() < 1e-6);
    assert_eq!(rs.seed, 2044445496);
    assert!((rs.fraction() - 0.825_622_9).abs() < 1e-6);
    assert_eq!(rs.seed as u32, 2842275331);
    for _ in 0..1000 { let f = rs.fraction(); assert!((0. ..1.).contains(&f)); }
}

#[test]
fn vrand_cone_polar_angle_is_the_wrapped_sphere_angle() {
    // The two draws of seed 12345 are u = 0.7168951 (azimuth) and v = 0.8256229; Phi = Acos(2v - 1) wrapped by Fmod into [0, half angle). The angle between the result
    // and the aim is exactly that wrapped Phi (independent of the azimuth).
    let aim = Vec3::new(0.3, 0.1, -1.).normalize();
    for half in [0.0317_f32, 0.009, 0.2] {
        let mut rs = RandomStream::new(12345);
        let r = vrand_cone(aim, half, half, &mut rs);
        let expect = ((2. * 0.825_622_9_f64 - 1.).acos()) % half as f64;
        assert!((r.length() - 1.).abs() < 1e-5);
        assert!((angle_between(r, aim) as f64 - expect).abs() < 2e-4, "half {half}: {} vs {expect}", angle_between(r, aim));
    }
    // no half angle: the (normalised) aim and no stream draw
    let mut rs = RandomStream::new(5);
    assert!((vrand_cone(aim * 3., 0., 0., &mut rs) - aim).length() < 1e-6 && rs.seed == 5);
    assert_eq!(fmod(1., 0.), 0.);
    assert!((fmod(7.5, 2.) - 1.5).abs() < 1e-6);
}

#[test]
fn vrand_cone_density_follows_the_exe_not_an_area_uniform_disc() {
    // Phi wraps uniformly in polar angle: mean angle -> half / 2 (an area-uniform cone would give 2 * half / 3), never above half.
    let aim = Vec3::new(0., 0., -1.);
    let half = 0.03_f32;
    let mut rs = RandomStream::new(777);
    let (mut sum, n) = (0f64, 20000);
    for _ in 0..n { let a = angle_between(vrand_cone(aim, half, half, &mut rs), aim); assert!(a <= half + 1e-5); sum += a as f64; }
    let mean = sum / n as f64;
    assert!((mean - 0.5 * half as f64).abs() < 0.03 * half as f64, "mean {mean} half {half}");
    // azimuth is uniform: the mean offset vector is ~ 0
}

// ---------------------------------------------------------------- spread / bloom

#[test]
fn heat_per_shot_is_curve_times_spread_and_clamped_to_the_curve_range() {
    let s = ar_row();
    let mut st = SpreadState::default();
    st.heat = 2.85;
    st.add_heat(&s, 1);
    assert!((st.heat - (2.85 + 8.75 * 0.15)).abs() < 1e-6, "{}", st.heat); // 4.1625
    st.add_heat(&s, 1);
    let y = 8.75 + (4.1625 - 4.1) / 1.4 * 1.25; // Eval at 4.1625 between (4.1, 8.75) and (5.5, 10)
    assert!((st.heat - (4.1625 + y * 0.15)).abs() < 1e-5 && (st.heat - 5.4834).abs() < 1e-3, "{}", st.heat);
    st.add_heat(&s, 1);
    assert_eq!(st.heat, 5.5, "clamped to HeatX3");
    // a heat below HeatX1 jumps up to HeatX1 (the minss/comiss clamp), e.g. the very first shot of a fresh actor
    let mut fresh = SpreadState::default();
    fresh.add_heat(&s, 1);
    assert_eq!(fresh.heat, 2.85);
}

#[test]
fn ar_held_fire_bloom_sequence_matches_the_decoded_formula() {
    // Frame loop at 60 Hz, trigger held (5.5 shots / s): tick(dt) then, when due, fire (LastFireTime, add_heat). The reference below is an independent, array-based
    // statement of the same decoded formula (piecewise-linear Y(heat), heat -= cool(heat) * dt once now - LastFireTime > RecoilRecoveryDelay, clamp to [X1, X3]).
    let s = ar_row();
    let dt = 1. / 60.;
    let mut st = settled(&s, SpreadInputs::default());
    assert_eq!(st.heat, 2.85);
    let (mut heat, mut last, mut next) = (2.85f32, -10f32, 10.5f32);
    let mut shots = vec![];
    for k in 0..(60 * 6) {
        let now = 10. + k as f32 * dt; // continue the clock after the settle phase
        st.tick(&s, &SpreadInputs { now, ..Default::default() }, dt, 1);
        if (now - last) > 0. { heat -= ref_y(&s.cool, heat) * dt; heat = heat.clamp(2.85, 5.5); }
        assert!((st.heat - heat).abs() < 1e-5, "frame {k}: {} vs {heat}", st.heat);
        if now + 1e-6 >= next {
            let y = ref_y(&s.heat, heat); heat = (heat + y * 0.15).clamp(2.85, 5.5); last = now;
            st.mark_fired(now); st.add_heat(&s, 1);
            shots.push(st.heat);
            assert!((st.heat - heat).abs() < 1e-5, "shot {}: {} vs {heat}", shots.len(), st.heat);
            next += 1. / 5.5;
        }
    }
    assert!(shots.len() >= 30);
    // held fire saturates at HeatX3 and the cone stays between its minimum and maximum: min 0.55 * 2.85 = 1.5675 deg, max 0.55 * 5.5 = 3.025 deg (full angle)
    assert!(shots.iter().skip(8).all(|h| *h > 5.2 && *h <= 5.5));
    assert!((st.spread_deg(0.) - st.mult * st.heat).abs() < 1e-6 && (st.mult - 0.55).abs() < 1e-3);
}

#[test]
fn stance_multipliers_combine_as_the_tick_does() {
    let s = ar_row();
    let base = SpreadInputs::default();
    let deg = |i: SpreadInputs| settled(&s, i).spread_deg(0.);
    let min_heat = 2.85;
    assert!((deg(base) - 0.55 * min_heat).abs() < 1e-4, "standing still (hip): StandingStillSpreadMultiplier x HeatX1");
    assert!((deg(SpreadInputs { ads_alpha: 1., targeting: true, ..base }) - 0.7 * 0.55 * min_heat).abs() < 1e-4, "ADS: Lerp(1, SpreadDownsights, alpha)");
    assert!((deg(SpreadInputs { ads_alpha: 0.5, ..base }) - 0.85 * 0.55 * min_heat).abs() < 1e-4);
    assert!((deg(SpreadInputs { speed_cm_s: 600., ..base }) - 1. * min_heat).abs() < 1e-4, "above MaxSpeed: movement factor 1");
    assert!((deg(SpreadInputs { speed_cm_s: 600., sprinting: true, ..base }) - 1.5 * min_heat).abs() < 1e-4, "sprinting multiplies the moving factor");
    assert!((deg(SpreadInputs { speed_cm_s: 95., ..base }) - (0.45 * 0.5 + 0.55) * min_heat).abs() < 1e-4, "between Min and Max speed: lerp from StandingStill to 1");
    assert!((deg(SpreadInputs { speed_cm_s: 89., ..base }) - 0.55 * min_heat).abs() < 1e-4, "below MinSpeed: still");
    assert!((deg(SpreadInputs { crouched: true, ..base }) - 0.8 * 0.55 * min_heat).abs() < 1e-4);
    assert!((deg(SpreadInputs { falling: true, speed_cm_s: 600., ..base }) - 1.25 * min_heat).abs() < 1e-4);
    // the Athena-only multipliers (crouch / jump / sprint) are not applied for a non-Athena owner
    assert!((deg(SpreadInputs { crouched: true, athena: false, ..base }) - 0.55 * min_heat).abs() < 1e-4);
}

#[test]
fn smoothed_factors_start_at_zero_and_ease_in_at_the_cool_rate() {
    // A freshly equipped weapon actor has zero movement factor: the first tick leaves mult at 0 and the factor approaches StandingStill at CoolY (2.7 /s) via FInterpTo.
    let s = ar_row();
    let mut st = SpreadState::default();
    st.tick(&s, &SpreadInputs::default(), 1. / 60., 1);
    assert!((st.movement - 0.55 * (2.7 / 60.)).abs() < 1e-6, "{}", st.movement);
    assert_eq!(st.heat, 2.85, "0 - rate*dt clamps up to HeatX1 on the first tick");
    assert!(!st.at_min, "movement factor not within 0.1 of StandingStill yet");
    for k in 1..120 { st.tick(&s, &SpreadInputs { now: k as f32 / 60., ..Default::default() }, 1. / 60., 1); }
    assert!(st.at_min);
}

#[test]
fn spread_cooling_waits_for_the_recoil_recovery_delay() {
    let mut s = ar_row(); s.recoil_recovery_delay = 0.5;
    let mut st = settled(&s, SpreadInputs::default());
    st.heat = 5.0; st.mark_fired(100.);
    st.tick(&s, &SpreadInputs { now: 100.4, ..Default::default() }, 1. / 60., 1);
    assert_eq!(st.heat, 5.0, "inside the delay");
    st.tick(&s, &SpreadInputs { now: 100.6, ..Default::default() }, 1. / 60., 1);
    assert!(st.heat < 5.0);
    // time dilation scales the elapsed time
    let mut st2 = settled(&s, SpreadInputs::default()); st2.heat = 5.0; st2.mark_fired(100.);
    st2.tick(&s, &SpreadInputs { now: 100.4, time_dilation: 2., ..Default::default() }, 1. / 60., 1);
    assert!(st2.heat < 5.0);
}

#[test]
fn ads_additional_cooldown_replaces_the_camera_weight_while_targeting() {
    let mut s = ar_row(); s.spread_downsights_additional_cooldown = 0.5;
    let mut st = SpreadState { movement: 0.55, crouch: 1., air: 1., heat: 2.85, ..Default::default() };
    let inp = SpreadInputs { targeting: true, ads_alpha: 1., targeting_since: 10., camera_blend_time: 0.25, now: 10.375, ..Default::default() };
    // alpha = (now - since) / (blend + cooldown) = 0.375 / 0.75 = 0.5 -> Lerp(1, 0.7, 0.5) * 0.55
    st.tick(&s, &inp, 0., 1);
    assert!((st.mult - 0.85 * 0.55).abs() < 1e-5, "{}", st.mult);
    st.tick(&s, &SpreadInputs { now: 20., ..inp }, 0., 1);
    assert!((st.mult - 0.7 * 0.55).abs() < 1e-5, "elapsed beyond the window: full ADS, {}", st.mult);
}
#[test]
fn perfect_aim_rules() {
    let s = ar_row();
    let rest = settled(&s, SpreadInputs::default());
    assert!(rest.at_min);
    // several pellets: only the first is exact, whatever the state
    assert!(rest.perfect_aim(0, 10, false, false, false) && !rest.perfect_aim(1, 10, true, true, true));
    // one bullet: needs at-min spread + the WID flag + exactly zero velocity + targeting
    assert!(rest.perfect_aim(0, 1, true, true, true));
    assert!(!rest.perfect_aim(0, 1, false, true, true) && !rest.perfect_aim(0, 1, true, false, true) && !rest.perfect_aim(0, 1, true, true, false));
    let mut moving = rest; moving.at_min = false;
    assert!(!moving.perfect_aim(0, 1, true, true, true));
}

// ---------------------------------------------------------------- fire: pellets, cone, damage

fn flat_shot() -> Shot { Shot { eye: Vec3::new(0., 1.75, 12.), forward: Vec3::new(0., -0.2, -1.).normalize() } }

#[test]
fn shotgun_fires_ten_pellets_first_exact_rest_inside_the_cone() {
    let g = gun(shotgun_row());
    let mut st = settled(&g.stats, SpreadInputs::default());
    let mut world = wor(vec![]);
    let shot = flat_shot();
    let ctx = FireContext { now: 100., ..Default::default() };
    let rep = fire(&g, &mut world, &shot, &mut st, &ctx, 12345);
    assert_eq!((rep.pellets, rep.dirs.len(), rep.impacts.len(), rep.misses), (10, 10, 10, 0));
    let full = 0.92 * 3.96; // StandingStill x HeatX1, degrees (full angle)
    assert!((rep.spread_deg - full).abs() < 1e-4);
    let half = full * 0.5 * 0.017453292;
    assert_eq!(rep.dirs[0], shot.forward, "pellet 0 of a multi-pellet cartridge is exactly on the aim ray and draws nothing from the stream");
    // pellet 1 consumed the first two draws of seed 12345: its polar angle is the wrapped sphere angle
    let expect = ((2. * 0.825_622_9_f64 - 1.).acos()) % half as f64;
    assert!((angle_between(rep.dirs[1], shot.forward) as f64 - expect).abs() < 3e-4);
    for d in &rep.dirs { assert!(angle_between(*d, shot.forward) <= half + 1e-5); }
    assert!(rep.dirs[1..].iter().any(|d| angle_between(*d, shot.forward) > 1e-4), "pellets spread");
    assert_eq!(st.heat, 5.28, "3.96 + 4.675 * 0.9091 saturates at HeatX3");
    assert_eq!(st.last_fire_time, 100.);
}

#[test]
fn ar_hip_shot_uses_one_cone_draw_and_ads_at_rest_is_perfect() {
    let g = gun(ar_row());
    let shot = flat_shot();
    let mut hip = settled(&g.stats, SpreadInputs::default());
    let mut world = wor(vec![]);
    let rep = fire(&g, &mut world, &shot, &mut hip, &FireContext { now: 100., ..Default::default() }, 12345);
    let half = 0.55 * 2.85 * 0.5 * 0.017453292;
    assert_eq!(rep.pellets, 1);
    assert!((angle_between(rep.dirs[0], shot.forward) as f64 - ((2. * 0.825_622_9_f64 - 1.).acos()) % half as f64).abs() < 3e-4);
    // ADS, standing still, velocity exactly zero, spread at its minimum: the AR's WID flag is false in the 1.8 data, a WID without the property keeps the class default true
    let ads_in = SpreadInputs { targeting: true, ads_alpha: 1., ..Default::default() };
    let mut ads = settled(&g.stats, ads_in);
    let ctx = FireContext { targeting: true, velocity_zero: true, now: 100., ..Default::default() };
    let mut off = gun(ar_row()); off.perfect_aim_when_targeting_min_spread = false;
    let r_off = fire(&off, &mut wor(vec![]), &shot, &mut ads.clone(), &ctx, 12345);
    assert_ne!(r_off.dirs[0], shot.forward, "flag false (the AR rows): no perfect aim");
    let r_on = fire(&g, &mut wor(vec![]), &shot, &mut ads, &ctx, 12345);
    assert_eq!(r_on.dirs[0], shot.forward, "flag true (class default): exact");
}

#[test]
fn damage_follows_the_five_key_falloff_curve() {
    let s = ar_row();
    let d = |r: f32| s.damage_at(DamageKind::Combat, r, 1);
    assert_eq!(d(0.), 32.);
    assert_eq!(d(1000.), 32.);
    assert_eq!(d(2500.), 32.);
    assert!((d(3125.) - (32. + (22.848 - 32.) * 0.5)).abs() < 1e-4 && (d(3125.) - 27.424).abs() < 1e-3);
    assert!((d(3750.) - 22.848).abs() < 1e-4);
    assert!((d(10000.) - 22.848).abs() < 1e-4 && (d(30000.) - 22.848).abs() < 1e-4);
    let e = |r: f32| s.damage_at(DamageKind::Environmental, r, 1);
    assert_eq!(e(100.), 35.);
    assert!((e(3000.) - (35. + (24.99 - 35.) * (500. / 1250.))).abs() < 1e-4);
    // shotgun: PB to 1250, Mid at 1500
    let sg = shotgun_row();
    assert_eq!(sg.damage_at(DamageKind::Combat, 1000., 1), 9.);
    assert!((sg.damage_at(DamageKind::Combat, 1375., 1) - (9. + (6.426 - 9.) * 0.5)).abs() < 1e-4);
    assert!((sg.damage_at(DamageKind::Combat, 5000., 1) - 6.426).abs() < 1e-4);
    // the level scale: with the scale column 0 and any level the value is unchanged; a non-zero DmgScale multiplies (Level - BaseLevel) * scale + 1
    assert_eq!(s.damage_at(DamageKind::Combat, 100., 5), 32.);
    let mut sc = ar_row(); sc.dmg_scale = 0.1;
    assert!((sc.damage_at(DamageKind::Combat, 100., 4) - 32. * 1.3).abs() < 1e-4);
    assert_eq!(sc.damage_at(DamageKind::Combat, 100., 1), 32.);
}

#[test]
fn headshot_uses_the_critical_zone_column() {
    let s = ar_row();
    assert_eq!(s.zone_damage(DamageKind::Combat, 1000., 1, Zone::Critical), 80.);
    assert_eq!(s.zone_damage(DamageKind::Combat, 1000., 1, Zone::Normal), 32.);
    assert!((s.zone_damage(DamageKind::Combat, 5000., 1, Zone::Critical) - 22.848 * 2.5).abs() < 1e-3);
    assert_eq!(s.zone_damage(DamageKind::Combat, 1000., 1, Zone::Vulnerability), 0.);
}

#[test]
fn fire_rate_burst_and_trace_distance() {
    let s = ar_row();
    assert!((s.fire_interval(1) - 1. / 5.5).abs() < 1e-7);
    let mut r = ar_row(); r.rof_scale = 0.1;
    assert!((r.fire_rate(3) - 5.5 * 1.2).abs() < 1e-5, "(max(level - BaseLevel, 0) * ROFScale + 1) * FiringRate");
    assert!((r.fire_rate(0) - 5.5).abs() < 1e-6);
    // the burst assault rifle row: 1.75 bursts / s, 3 cartridges per fire at 12 / s
    let mut b = ar_row(); b.firing_rate = 1.75; b.burst_firing_rate = 12.; b.cartridge_per_fire = 3;
    assert!((b.fire_interval(1) - 1. / 1.75).abs() < 1e-6 && (b.burst_interval(1) - 1. / 12.).abs() < 1e-7);
    // BurstFiringRate <= 0: FireRate * (CartridgePerFire + 1)
    b.burst_firing_rate = 0.;
    assert!((b.burst_rate(1) - 1.75 * 4.).abs() < 1e-5);
    assert!((ar_row().auto_reload_delay() - 0.3001).abs() < 1e-4);
    let mut o = ar_row(); o.auto_reload_delay_override = 0.8;
    assert_eq!(o.auto_reload_delay(), 0.8);
    assert_eq!(s.trace_distance_cm(), 25000.);
    assert_eq!(s.trace_range_m(), 250.);
    let mut short = ar_row(); short.rng = [1., 1., 1., 50.];
    assert_eq!(short.trace_range_m(), 1., "floored at 100 cm");
}

// ---------------------------------------------------------------- aim source: camera vs muzzle

#[test]
fn bullets_leave_the_camera_unless_the_muzzle_is_blocked_at_close_range() {
    let mut g = gun(ar_row());
    g.pres.aim_from_muzzle_at_close_range = true;
    let eye = Vec3::new(0., 1.0, 12.);
    let fwd = Vec3::new(0., 0., -1.);
    let muzzle = Vec3::new(0.25, 0.9, 11.4);
    let rock = vec![Harvestable::new(Kind::Rock, Vec3::new(0., 0., 8.))]; // 4 m ahead, 1.4 m high
    let a = select_aim(&g, &rock, eye, fwd, Some(muzzle), false);
    assert!(a.from_muzzle, "the muzzle trace (6.5 m) is blocked by the rock and the flag is set");
    assert_eq!(a.start, muzzle);
    let target = eye + fwd * trace_world(&rock, eye, fwd, 250., 0.).unwrap().t;
    assert!((a.dir - (target - muzzle).normalize()).length() < 1e-5, "muzzle aimed at the camera trace's hit point");
    // flag off, no muzzle given, scope targeting, or nothing in the way: the camera ray
    let mut off = gun(ar_row()); off.pres.aim_from_muzzle_at_close_range = false;
    assert!(!select_aim(&off, &rock, eye, fwd, Some(muzzle), false).from_muzzle);
    assert_eq!(select_aim(&g, &rock, eye, fwd, None, false), AimSelectionRef::camera(eye, fwd));
    let mut scope = gun(ar_row()); scope.pres.aim_from_muzzle_at_close_range = true; scope.pres.use_scope_targeting = true;
    assert!(!select_aim(&scope, &rock, eye, fwd, Some(muzzle), true).from_muzzle && select_aim(&scope, &rock, eye, fwd, Some(muzzle), false).from_muzzle);
    let far = vec![Harvestable::new(Kind::Rock, Vec3::new(0., 0., -20.))];
    assert!(!select_aim(&g, &far, eye, fwd, Some(muzzle), false).from_muzzle);
}
struct AimSelectionRef;
impl AimSelectionRef { fn camera(eye: Vec3, dir: Vec3) -> shoot::AimSelection { shoot::AimSelection { start: eye, dir, from_muzzle: false, muzzle_blocked_at: None } } }

#[test]
fn falloff_damage_is_applied_to_props_by_range() {
    let g = gun(ar_row());
    // a tree 29.6 m from the camera: EnvDmg falls between RngPB (25 m) and RngMid (37.5 m)
    let mut world = wor(vec![Harvestable::new(Kind::Tree, Vec3::new(0., 0., -18.))]);
    let shot = Shot { eye: Vec3::new(0., 1.75, 12.), forward: Vec3::new(0., 0., -1.) };
    let mut st = SpreadState::default();
    let rep = fire(&g, &mut world, &shot, &mut st, &FireContext::default(), 1);
    let i = &rep.impacts[0];
    assert!((i.distance_m - 29.6).abs() < 1e-3);
    assert!((i.damage - (35. + (24.99 - 35.) * ((2960. - 2500.) / 1250.))).abs() < 1e-3, "{}", i.damage);
    assert_eq!(world.props[0].health, 600. - i.damage);
}

// ---------------------------------------------------------------- the install (skipped without FN_TEST_ROOT)

const WID: [&str; 3] = [
    "/Game/Athena/Items/Weapons/WID_Assault_Auto_Athena_C_Ore_T02",
    "/Game/Athena/Items/Weapons/WID_Shotgun_Standard_Athena_C_Ore_T03",
    "/Game/Athena/Items/Weapons/WID_Sniper_BoltAction_Scope_Athena_R_Ore_T03",
];
fn paks() -> Option<&'static fn_pak::PakSet> {
    let root = fn_live::var_os("FN_TEST_ROOT")?;
    let paks = fn_pak::PakSet::open_install(&root).unwrap();
    Some(Box::leak(Box::new(paks)))
}
fn guns() -> Option<Vec<GunHit>> {
    let paks = paks()?;
    Some(WID.iter().map(|w| load(&ClipSourceDir::Pak(paks), w).unwrap()).collect())
}

#[test]
fn stat_rows_wire_blueprints_and_projectile_are_read_from_the_install() {
    let Some(g) = guns() else { eprintln!("FN_TEST_ROOT not set, skipping"); return };
    let ar = &g[0];
    assert_eq!(ar.row, "Assault_Auto_Athena_C_Ore_T03");
    assert_eq!((ar.stats.spread, ar.stats.spread_downsights, ar.stats.standing_still_spread_mult, ar.stats.crouching_spread_mult, ar.stats.jumping_falling_spread_mult, ar.stats.sprinting_spread_mult), (0.15, 0.7, 0.55, 0.8, 1.25, 1.5));
    assert_eq!((ar.stats.min_speed_for_spread_mult, ar.stats.max_speed_for_spread_mult), (90., 100.));
    assert_eq!(ar.stats.heat, ar_row().heat);
    assert_eq!(ar.stats.cool, ar_row().cool);
    assert_eq!((ar.stats.base_level, ar.stats.cartridge_per_fire, ar.stats.burst_firing_rate, ar.stats.rof_scale), (1, 1, 5., 0.));
    assert!(!ar.perfect_aim_when_targeting_min_spread, "the AR WIDs serialize bShouldUsePerfectAimWhenTargetingMinSpread = false");
    assert_eq!(ar.stats.athena_recoil_magnitude, (4., 4.));
    assert_eq!((ar.stats.athena_recoil_angle, ar.stats.athena_recoil_interp_speed, ar.stats.athena_recoil_recovery_interp_speed, ar.stats.athena_recoil_downsights_mult), ((-25., 25.), 12., 6., 0.5));
    assert_eq!((ar.stats.dmg[0], ar.stats.env_dmg[0], ar.stats.rng, ar.stats.zone_critical, ar.stats.trace_range_m()), (32., 35., [2500., 3750., 25000., 25000.], 2.5, 250.));
    assert!((ar.stats.env_dmg[1] - 24.99).abs() < 1e-3);
    assert!(ar.pres.aim_from_muzzle_at_close_range && ar.pres.maintain_aim_during_targeting && !ar.pres.use_scope_targeting);
    assert!(ar.pres.tracer_template.as_deref().unwrap().contains("TracerGeneric_Athena"));
    assert!(ar.pres.impact_sound(1).unwrap().contains("WOOD_Bullet_Impact") && ar.pres.impact_sound(2).unwrap().contains("STONE_Bullet_Impact"));
    assert!(ar.pres.impact_effect(1).unwrap().contains("P_Impact_Wood") && ar.pres.impact_effect(2).unwrap().contains("P_Impact_Stone") && ar.pres.impact_effect(0).unwrap().contains("P_Impact_Default"));
    assert_eq!(ar.ability.package, "/Game/Abilities/Weapons/Ranged/GA_Ranged_GenericDamage");
    assert_eq!((ar.ability.selections[0].shape.as_str(), ar.ability.selections[0].range, ar.ability.selections[0].exclude_obstructed_by_world, ar.ability.selections[0].max_targets), ("EFortTargetSelectionShape::Cone", 5000., true, -1));
    assert_eq!(ar.ability.impact_cues, ["GameplayCue.Impact.Physical"]);
    assert_eq!(ar.ability.weapon_fire_cue, ["GameplayCue.Weapons.Activation"]);
    assert!(ar.projectile.is_none());
    let sg = &g[1];
    assert_eq!((sg.stats.bullets_per_cartridge, sg.stats.dmg[0], sg.stats.env_dmg[0], sg.stats.rng[3], sg.stats.sweep_radius), (10, 9., 10., 25000., 10.));
    let sn = &g[2];
    let pr = sn.projectile.as_ref().expect("sniper is a projectile weapon");
    assert_eq!((pr.initial_speed, pr.max_speed, pr.initial_speed_row.as_str()), (30000., 30000., "Sniper_BoltAction_InitialSpeed"));
    assert_eq!((sn.stats.spread_downsights, sn.stats.dmg[0], sn.stats.env_dmg[0]), (0., 105., 105.));
    assert!(sn.pres.use_scope_targeting);
    // the synthetic rows used above equal the shipped rows
    assert_eq!(ar.stats.spread, ar_row().spread);
    assert_eq!(sg.stats.heat, shotgun_row().heat);
    assert_eq!(sg.stats.cool, shotgun_row().cool);
}

#[test]
fn every_br_weapon_loads_and_has_a_sane_firing_model() {
    let Some(paks) = paks() else { return };
    let src = ClipSourceDir::Pak(paks);
    let wids = br_weapon_wids(paks);
    assert!(wids.len() >= 44, "{} BR weapon definitions", wids.len());
    let mut loaded = 0;
    let mut perfect_default_true = vec![];
    for w in &wids {
        let g = match load(&src, w) { Ok(g) => g, Err(e) => { eprintln!("skip {w}: {e}"); continue } }; // WIDs that inherit their stat handle are listed separately below
        loaded += 1;
        let s = &g.stats;
        assert!(s.rng.windows(2).all(|p| p[0] <= p[1]), "{w}: ranges ascend {:?}", s.rng);
        assert!(s.heat.windows(2).all(|p| p[0].0 <= p[1].0) && s.cool.windows(2).all(|p| p[0].0 <= p[1].0), "{w}: heat/cool x ascend");
        assert!(s.firing_rate > 0. && s.bullets_per_cartridge >= 1 && s.cartridge_per_fire >= 1 && s.clip_size >= 1, "{w}");
        assert!(!s.dmg_scale_table && !s.env_dmg_scale_table, "{w}: no scale tables in 1.8 BR rows");
        // falloff never increases with range
        let mut prev = f32::MAX;
        for r in [0., 500., 1250., 2500., 3000., 3750., 8000., 25000., 40000.] { let d = s.damage_at(DamageKind::Combat, r, s.base_level); assert!(d <= prev + 1e-4 && d >= 0., "{w}: {r}"); prev = d; }
        // a rested hip-fire weapon sits at HeatX1 and the standing-still factor
        let st = settled(s, SpreadInputs::default());
        assert!((st.heat - s.cool[0].0).abs() < 1e-5 || s.cool[0].0 == 0., "{w}");
        assert!((st.mult - s.standing_still_spread_mult).abs() < 1e-3, "{w}: {} vs {}", st.mult, s.standing_still_spread_mult);
        // one shot raises the heat inside [X1, X3] or leaves a flat zero curve alone
        let mut after = st; after.add_heat(s, s.base_level);
        assert!(after.heat >= st.heat - 1e-6 && after.heat <= s.heat[2].0.max(st.heat) + 1e-5, "{w}");
        if g.perfect_aim_when_targeting_min_spread { perfect_default_true.push(w.rsplit('/').next().unwrap().to_owned()); }
    }
    assert!(loaded >= 40, "{loaded} loaded");
    assert!(perfect_default_true.iter().any(|n| n.contains("Sniper_AMR")) && perfect_default_true.iter().any(|n| n.contains("Hydraulic")), "{perfect_default_true:?}");
    assert!(!perfect_default_true.iter().any(|n| n.contains("WID_Assault_Auto_Athena_C")), "{perfect_default_true:?}");
}

#[test]
fn bullets_damage_props_through_their_health_with_surface_events() {
    let Some(g) = guns() else { return };
    let mut world = wor(vec![Harvestable::new(Kind::Tree, Vec3::new(0., 0., 6.))]);
    let shot = Shot { eye: Vec3::new(0., 1.75, 12.), forward: Vec3::new(0., 0., -1.) };
    let mut st = SpreadState::default(); // never ticked: zero cone
    let ctx = FireContext::default();
    let rep = fire(&g[0], &mut world, &shot, &mut st, &ctx, 7);
    assert_eq!((rep.pellets, rep.misses, rep.impacts.len()), (1, 0, 1));
    let i = &rep.impacts[0];
    assert_eq!((i.prop, i.damage, i.surface, i.destroyed), (Some(0), 35., 1, false));
    assert!((i.distance_m - 5.6).abs() < 1e-3, "tree trunk radius 0.4 at z=6 -> hit at z=6.4");
    assert!(i.sound.as_deref().unwrap().contains("WOOD_Bullet_Impact") && i.effect.as_deref().unwrap().contains("P_Impact_Wood"));
    assert_eq!(world.props[0].health, 565.);
    for _ in 0..17 { fire(&g[0], &mut world, &shot, &mut st, &ctx, 7); }
    assert!(!world.props[0].alive(), "18 x 35 = 630 >= 600");
    assert!(world.fx.events.iter().any(|e| matches!(e, FxEvent::Broke { object: _, .. })));
    // ground
    let down = Shot { eye: Vec3::new(20., 1.75, 12.), forward: Vec3::new(0., -1., -1.).normalize() };
    let r = fire(&g[0], &mut world, &down, &mut st, &ctx, 7);
    assert!(r.impacts[0].prop.is_none() && (r.impacts[0].pos.y).abs() < 1e-4 && r.impacts[0].surface == 0);
    // shotgun: 10 pellets x EnvDmg 10 at point blank with a never-ticked (zero) cone
    let mut w2 = wor(vec![Harvestable::new(Kind::Rock, Vec3::new(0., 0., 6.))]);
    let mut st2 = SpreadState::default();
    let r = fire(&g[1], &mut w2, &Shot { eye: Vec3::new(0., 0.7, 9.), forward: Vec3::new(0., 0., -1.) }, &mut st2, &ctx, 7);
    assert_eq!((r.pellets, r.impacts.len(), r.impacts[0].surface), (10, 10, 2));
    assert_eq!(w2.props[0].health, 450. - 100.);
}

#[test]
fn sniper_projectile_flies_at_the_recovered_speed_and_damages_a_rock() {
    let Some(g) = guns() else { return };
    let sn = &g[2];
    let mut world = wor(vec![Harvestable::new(Kind::Rock, Vec3::new(0., 0., -90.))]);
    let shot = Shot { eye: Vec3::new(0., 1.0, 12.), forward: Vec3::new(0., 0., -1.) };
    let mut st = SpreadState::default();
    let mut p = Projectile::launch(sn, &world, &shot, &mut st, &FireContext::default(), 1).unwrap();
    assert!((p.vel.length() - 300.).abs() < 1e-3);
    let mut hit = None; let mut t = 0.0f32;
    while hit.is_none() && !p.dead() && t < 5. { hit = p.step(sn, &mut world, 1. / 120., 0., 400., 1); t += 1. / 120.; }
    let h = hit.expect("rock hit");
    assert_eq!((h.prop, h.damage, h.surface), (Some(0), 105., 2));
    assert!((t - (12. + 90. - 1.0) / 300.).abs() < 0.02, "flight time {t}");
    assert_eq!(world.props[0].health, 450. - 105.);
}



/// TERRAIN: a shot down a hillside hits the landscape where the ray meets it (a 20 deg slope rising along +x, 3 m high at x = 0), with its face normal.
#[test] fn a_shot_hits_the_landscape_not_the_plate() {
    let k = 20f32.to_radians().tan();
    let hf = fn_terrain::Heightfield::synthetic(glam::Vec2::splat(-3000.), 100., 2, 31, |x, _| 300. + k * x);
    let w = fn_terrain::World { hf, map: fn_terrain::Mapping { centre_cm: [0.; 3], anchor: Vec3::ZERO, ref_z_cm: 0. } };
    let h = crate::shoot::trace_world_on(Some(&w), &[], Vec3::new(0., 10., 0.), Vec3::NEG_Y, 50., 0.).unwrap();
    assert!((h.t - 7.).abs() < 0.01 && h.prop.is_none() && (h.normal - Vec3::new(-20f32.to_radians().sin(), 20f32.to_radians().cos(), 0.)).length() < 1e-3, "{h:?}");
    // the plate below the landscape is never hit: aiming at y = 0 under a 3 m hill stops on the hill
    let h = crate::shoot::trace_world_on(Some(&w), &[], Vec3::new(-10., 5., 0.), Vec3::new(1., -0.5, 0.).normalize(), 50., 0.).unwrap();
    let p = Vec3::new(-10., 5., 0.) + Vec3::new(1., -0.5, 0.).normalize() * h.t;
    assert!((p.y - w.ground_y(p.x, p.z).unwrap()).abs() < 0.01 && p.y > 0.5, "{p:?}");
}

// ---------------------------------------------------------------- COMPLEX COLLISION (2026-10-07): bullets hit the render triangles (bTraceComplex WeaponTrace)
#[test]
fn bullets_hit_the_complex_triangles_and_pass_the_opening_the_simple_box_covers() {
    use fn_combat::{Hull, HullSet, TriMesh, Resource};
    use std::sync::Arc;
    // one simple box over the whole 4 m x 3 m arch (a body whose simple collision bridges its opening, like the cooked picnic table / shelves) ...
    let mut v = vec![]; for x in [-2f32, 2.] { for y in [0f32, 3.] { for z in [7.8f32, 8.2] { v.push(Vec3::new(x, y, z)); } } }
    let mut p = Harvestable::real(Kind::Tree, Resource::Wood, Vec3::new(0., 0., 8.), 0., 1., 300., 0, Some(Arc::new(HullSet::new(vec![Hull::from_points(&v).unwrap()]))));
    let ray = |props: &[Harvestable], x: f32, y: f32| crate::shoot::trace_props(props, Vec3::new(x, y, 0.), Vec3::Z, 20.);
    assert!(ray(&[p.clone()], 0., 1.).is_some(), "without triangles the box answers");
    // ... and its render triangles: pillars x in -2..-1 / 1..2, lintel y 2..3, at z 8
    let quad = |x0: f32, x1: f32, y0: f32, y1: f32| { let (a, b, c, d) = (Vec3::new(x0, y0, 8.), Vec3::new(x1, y0, 8.), Vec3::new(x1, y1, 8.), Vec3::new(x0, y1, 8.)); vec![[a, b, c], [a, c, d]] };
    let mut t = quad(-2., -1., 0., 3.); t.extend(quad(1., 2., 0., 3.)); t.extend(quad(-1., 1., 2., 3.));
    p.complex = Some(Arc::new(TriMesh::new(t)));
    assert!(ray(&[p.clone()], 0., 1.).is_none(), "the bullet passes the opening");
    let (i, d, n) = ray(&[p.clone()], 1.5, 1.).unwrap();
    assert!(i == 0 && (d - 8.).abs() < 1e-4 && n == Vec3::NEG_Z, "{d} {n}");
    assert!(ray(&[p.clone()], 0., 2.5).is_some(), "the lintel");
    p.health = 0.; assert!(ray(&[p], 1.5, 1.).is_none(), "a destroyed prop stops nothing");
}

#[test]
fn tracer_classes_are_read_from_the_install() {
    let Some(g) = guns() else { eprintln!("FN_TEST_ROOT not set, skipping"); return };
    let ar = g[0].tracer.as_ref().expect("AR tracer");
    let sg = g[1].tracer.as_ref().expect("shotgun tracer");
    eprintln!("AR {ar:?}
SG {sg:?}");
    assert!(ar.mesh.contains("SM_tracer_01") && sg.mesh.contains("SM_Shotgun_Tracer"));
    assert_eq!((ar.mesh_scale_mult, ar.mesh_scale_time, ar.speed_scale_mult), ([8., 1., 1.], 0.05, (2., 2.)));
    assert_eq!((sg.mesh_scale_mult[0], sg.speed_scale_mult, sg.scale_speed, sg.scale_on_death), (5., crate::tracer::DEFAULT_SPEED_SCALE_MULT, true, true));
    assert!((ar.mesh_min_x + 162.66).abs() < 0.01 && ar.mesh_max_x.abs() < 0.01);
    assert!(ar.speed > 1000. && ar.speed_row == "TracerGeneric_Speed");
    assert!(ar.pass_by_far_sound.as_deref().unwrap().contains("bullet_whip_Cue") && ar.pass_by_radii == (100., 400.));
    assert!(g[2].pres.impact_sound(1).is_none() && g[2].pres.impact_sound(0).is_none() && g[0].pres.impact_sound(1).is_some(), "the sniper CDO nulls the surface sounds: {:?}", g[2].pres.impact_sounds);
    let sn = &g[2].projectile_particles;
    assert!(sn.iter().any(|(t, l)| t.contains("P_SR_Ribbon_SmokeTrail") && l[0] == -4.) && sn.iter().any(|(t, _)| t.contains("P_Throwing_Star_HandTrail")), "{sn:?}");
}
