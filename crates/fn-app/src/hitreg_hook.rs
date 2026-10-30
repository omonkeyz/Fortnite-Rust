//! HITREG HOOK (fn-app): glue between the arms (shots fired) and `fn-hitreg` (trace / spread / damage / prop health). The LOGIC is `fn_hitreg`; this file loads the three
//! firearms' hit data from the install on a worker thread, turns every shot the arms report into a cartridge fired from the camera ray at the props of `sim.world`, flies
//! sniper projectiles, applies the recovered recoil (`fn_weapons::recovered_recoil`, RECOIL HOOK) to the look delta and queues the impacts (`take_impacts`) for audio / particles.
use crate::arms::Arms;
use fn_game::{FrameInput, GameSim};
use fn_hitreg::{fire, load, FireContext, GunHit, Impact, Projectile, Shot, SpreadInputs, SpreadState};
use fn_weapons::recovered_recoil::{CrtRand, RecoilState, RecoilStats}; // RECOIL HOOK: recovered native recoil (fn-weapons/RECOIL.md)
use std::sync::Mutex;

struct State { guns: Vec<GunHit>, spread: Vec<SpreadState>, held: Option<usize>, recoil: Vec<RecoilState>, crt: CrtRand, now: f32, last: Vec<u32>, flying: Vec<(usize, Projectile, u64)> }
/// Firearms installed after start-up (`weapon_runtime`): (gun index, hit data read by the weapon's load worker); appended in gun-index order once the starters' state exists.
static EXTRA: Mutex<Vec<(usize, GunHit)>> = Mutex::new(Vec::new());
pub fn add_gun(g: usize, hit: GunHit) { if let Ok(mut q) = EXTRA.lock() { q.push((g, hit)); } }
static GUNS: Mutex<Option<Vec<GunHit>>> = Mutex::new(None);
static STARTED: Mutex<bool> = Mutex::new(false);
static STATE: Mutex<Option<State>> = Mutex::new(None);
/// (targeting camera blend weight, muzzle world position) of this frame, set by the app before `on_arms`.
static AIM_INPUTS: Mutex<(f32, Option<glam::Vec3>)> = Mutex::new((0., None));
pub fn set_aim_inputs(ads_weight: f32, muzzle: Option<glam::Vec3>) { if let Ok(mut a) = AIM_INPUTS.lock() { *a = (ads_weight, muzzle); } }
static IMPACTS: Mutex<Vec<(usize, Impact)>> = Mutex::new(Vec::new());
/// RETICLE HOOK: the held firearm's `heat * multiplier` (degrees) after this frame's spread tick: exactly the product `SFortReticle::Tick` (RVA 0x14305c0f0) reads from the
/// weapon (`+0xC10` heat via 0x923f40, `+0xC24` multiplier via 0x923f60) before halving it. 0 while no firearm is held or the hit data is still loading.
static RETICLE_SPREAD: Mutex<f32> = Mutex::new(0.);
pub fn reticle_spread_deg() -> f32 { RETICLE_SPREAD.lock().map_or(0., |v| *v) }
/// BUILDING HUD: `AFortWeaponRanged::GetRange` (@140929630: max(`RngLong`, `RngMax`)) of the held firearm, metres. None = no firearm held.
pub fn held_gun_range_m() -> Option<f32> {
    let st = STATE.lock().ok()?;
    let s = st.as_ref()?;
    let r = s.guns.get(s.held?)?.stats.rng;
    Some(r[2].max(r[3]) / 100.)
}

/// Impacts since the last call: (gun index 0..3, impact). `Impact::effect` / `sound` are the weapon actor's `ImpactPhysicalSurfaceEffects` / `Sounds` package paths.
pub fn take_impacts() -> Vec<(usize, Impact)> { IMPACTS.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default() }

fn start(install: std::path::PathBuf, wids: Vec<String>) {
    let Ok(mut s) = STARTED.lock() else { return };
    if *s { return; }
    *s = true;
    std::thread::spawn(move || {
        let paks = match fn_pak::PakSet::open_install(&install) { Ok(p) => p, Err(e) => { eprintln!("[hitreg] unavailable: {e}"); return } };
        let src = fn_pose::ClipSourceDir::Pak(&paks);
        let mut guns = vec![];
        for w in &wids { match load(&src, w) { Ok(g) => guns.push(g), Err(e) => { eprintln!("[hitreg] {w}: {e}"); return } } }
        for g in &guns { eprintln!("[hitreg] {} row {}: trace {} m, spread {} deg x multipliers, EnvDmg {} / DmgPB {}, headshot x{}, {} pellet(s){}", g.wid.rsplit('/').next().unwrap_or(""), g.row, g.stats.trace_range_m(), g.stats.spread, g.stats.env_dmg[0], g.stats.dmg[0], g.stats.zone_critical, g.stats.bullets_per_cartridge, g.projectile.as_ref().map_or(String::new(), |p| format!(", projectile {} cm/s", p.initial_speed))); }
        if let Ok(mut slot) = GUNS.lock() { *slot = Some(guns); }
    });
}

/// Once per frame after `arms.update` (the first call starts the load): `aiming` = the ADS driver's state.
static INSTALL: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
/// The game install (called once when the loadout resources arrive).
pub fn set_install(dir: Option<std::path::PathBuf>) { if let Ok(mut i) = INSTALL.lock() { *i = dir; } }
pub fn gun_by_row(row: &str) -> Option<GunHit> { STATE.lock().ok()?.as_ref()?.guns.iter().find(|g| g.row == row).cloned() }
pub fn install_dir() -> Option<std::path::PathBuf> { INSTALL.lock().ok().and_then(|i| i.clone()) }
/// RECOIL HOOK: `inp` = this frame's look input (the `DeltaRot` of `AFortPlayerController::UpdateRotation`), `gamepad` = `IsUsingGamepad`.
pub fn on_arms(sim: &mut GameSim, arms: &Arms, aiming: bool, dt: f32, inp: &mut FrameInput, gamepad: bool) {
    fn_hitreg::shoot::set_pawn_damage_gate(sim.damage_enabled());
    let install = INSTALL.lock().ok().and_then(|i| i.clone());
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if st.is_none() {
        if let Some(dir) = install { start(dir, arms.data.guns.iter().take(fn_pose::loadout_gun::STARTER_GUNS).map(|g| g.def.item.clone()).collect()); }
        let Some(guns) = GUNS.lock().ok().and_then(|mut g| g.take()) else { return };
        let n = guns.len();
        *st = Some(State { guns, spread: vec![SpreadState::default(); n], held: None, recoil: vec![RecoilState::default(); n], crt: CrtRand::new(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.subsec_nanos())), now: 0., last: arms.shots.iter().take(n).copied().collect(), flying: vec![] });
    }
    let s = st.as_mut().expect("loaded");
    // firearms installed after start-up join the state in gun-index order
    if let Ok(mut q) = EXTRA.lock() {
        q.sort_by_key(|(g, _)| *g);
        while let Some(g) = q.first().map(|(g, _)| *g) {
            if g != s.guns.len() { break; }
            let (_, hit) = q.remove(0);
            s.guns.push(hit); s.spread.push(SpreadState::default()); s.recoil.push(RecoilState::default()); s.last.push(arms.shots.get(g).copied().unwrap_or(0));
        }
    }
    let c = &sim.controller;
    let (ads_weight, muzzle) = AIM_INPUTS.lock().map(|a| *a).unwrap_or((0., None));
    // 3-D speed in cm/s (the exe's |GetVelocity()|), exactly-zero velocity for the perfect-aim rule
    let speed_cm_s = (c.velocity.length_squared() + c.vertical_speed * c.vertical_speed).sqrt() * 100.;
    let shot = Shot { eye: sim.cam_view.eye, forward: sim.cam_view.forward };
    s.now += dt; // World->TimeSeconds
    let ctx = FireContext { targeting: aiming, velocity_zero: c.velocity.length_squared() == 0. && c.vertical_speed == 0., muzzle, level: 1, now: s.now };
    // SPREAD HOOK: AFortWeaponRanged::Tick (RVA 0x93f8b0) of the held weapon actor; a weapon actor is spawned on equip, so switching guns restarts its spread state (INFERENCE)
    let held = arms.gun_index().filter(|g| *g < s.guns.len()); // a firearm whose hit data has not joined yet fires nothing and has no spread / recoil (never the case for installed packs: they join with their data)
    if held != s.held { s.held = held; if let Some(g) = held { s.spread[g] = SpreadState::default(); } }
    if let Some(g) = held {
        let inp = SpreadInputs { now: s.now, speed_cm_s, crouched: c.crouched, falling: c.airborne(), sprinting: c.running && !c.crouched && speed_cm_s > 1., targeting: aiming, ads_alpha: ads_weight,
            targeting_since: s.now, camera_blend_time: 0., time_dilation: 1., athena: true };
        s.spread[g].tick(&s.guns[g].stats, &inp, dt, 1);
    }
    for g in 0..s.guns.len().min(arms.shots.len()) {
        let n = arms.shots[g].saturating_sub(s.last[g]);
        s.last[g] = arms.shots[g];
        for _ in 0..n {
            let gun = &s.guns[g];
            crate::muzzle_hook::push_fire(g); // MUZZLE HOOK: OnPlayWeaponFireFX of the shot
            let seed = s.crt.next(); // FRandomStream seed = rand(), drawn at the top of the fire function (RVA 0x93372e)
            // RECOIL HOOK: AddRecoil (RVA 0x917930) once per shot; LastFireTime = TimeSeconds
            let crt = &mut s.crt; s.recoil[g].on_fire(s.now); s.recoil[g].add_recoil(&recoil_stats(&gun.stats), aiming, gamepad, ATHENA_RECOIL, &mut || crt.next());
            if fn_hitreg::explosive::def_of(gun).is_some() { if let Some(l) = fn_hitreg::explosive::launch(gun, &sim.world, &shot, &mut s.spread[g], &ctx, seed) { crate::rocket_hook::fire_local(gun, l); } continue; }
            if let Some(p) = Projectile::launch(gun, &sim.world, &shot, &mut s.spread[g], &ctx, seed) { crate::mp_hook::on_cartridge(&gun.row, shot.eye, shot.forward, &[]); let key = crate::gunfx_hook::projectile_spawn(gun, p.pos, p.vel); s.flying.push((g, p, key)); continue; }
            let mut pawns = crate::mp_hook::pawn_targets();
            pawns.extend(crate::bots_hook::pawn_targets());
            let rep = if pawns.is_empty() { fire(gun, &mut sim.world, &shot, &mut s.spread[g], &ctx, seed) } else { fn_hitreg::shoot::fire_at(gun, &mut sim.world, &mut pawns, &crate::mp_hook::pawn_fire(), &shot, &mut s.spread[g], &ctx, seed) };
            crate::bots_hook::on_player_hits(&gun.row, &rep.pawn_hits);
            let mp_hits: Vec<_> = rep.pawn_hits.iter().filter(|h| !crate::bots_hook::is_bot(h.id)).cloned().collect();
            crate::mp_hook::on_cartridge(&gun.row, shot.eye, shot.forward, &mp_hits);
            crate::supply_drop_hook::on_shot(shot.eye, shot.forward, rep.impacts.iter().map(|i| i.distance_m).reduce(f32::min), gun.stats.dmg[0] * rep.pellets.max(1) as f32);
            eprintln!("[hitreg f{:05}] {} fired {} pellet(s) spread {:.2} deg{}: {} impact(s) {} miss(es){}", sim.frames, gun.row, rep.pellets, rep.spread_deg, if rep.from_muzzle { " (muzzle)" } else { "" }, rep.impacts.len(), rep.misses,
                rep.impacts.first().map_or(String::new(), |i| format!("; first {:.1} m prop {:?} surface {} damage {} destroyed {}", i.distance_m, i.prop, i.surface, i.damage, i.destroyed)));
            let start = crate::muzzle_hook::muzzle_ue(g).unwrap_or_else(|| glam::Vec3::from(fn_particles::sim::game_to_ue(rep.start.to_array())));
            crate::gunfx_hook::on_shot(gun, &rep.tracer_ends, start, false);
            for i in &rep.impacts { crate::gunfx_hook::on_impact(gun, i); }
            if let Ok(mut q) = IMPACTS.lock() { q.extend(rep.impacts.into_iter().map(|i| (g, i))); }
        }
    }
    // sniper projectiles (fixed 1/120 s sub-steps)
    let (grav, mut left) = (fn_hitreg::shoot::projectile_gravity_m_s2(), dt);
    while left > 1e-6 && !s.flying.is_empty() {
        let h = left.min(1. / 120.); left -= h;
        let mut done = vec![];
        for (i, (g, p, key)) in s.flying.iter_mut().enumerate() {
            let gun = &s.guns[*g];
            let mut pawns = crate::mp_hook::pawn_targets();
            pawns.extend(crate::bots_hook::pawn_targets());
            let (hit, phits) = if pawns.is_empty() { (p.step(gun, &mut sim.world, h, grav, 1000., 1), vec![]) } else { p.step_at(gun, &mut sim.world, &mut pawns, &crate::mp_hook::pawn_fire(), h, grav, 1000., 1) };
            crate::bots_hook::on_player_hits(&gun.row, &phits);
            let phits: Vec<_> = phits.into_iter().filter(|h| !crate::bots_hook::is_bot(h.id)).collect();
            if !phits.is_empty() { crate::mp_hook::on_projectile_hits(&gun.row, &phits); }
            if let Some(imp) = hit {
                eprintln!("[hitreg f{:05}] {} projectile hit {:.1} m prop {:?} surface {} damage {} destroyed {}", sim.frames, gun.row, imp.distance_m, imp.prop, imp.surface, imp.damage, imp.destroyed);
                crate::gunfx_hook::on_impact(gun, &imp);
                crate::gunfx_hook::projectile_end(*key, gun, Some((imp.pos, imp.surface == fn_hitreg::shoot::SURFACE_PLAYER)));
                if let Ok(mut q) = IMPACTS.lock() { q.push((*g, imp)); }
                done.push(i);
            } else if p.dead() { crate::gunfx_hook::projectile_end(*key, gun, None); done.push(i); }
        }
        for i in done.into_iter().rev() { s.flying.swap_remove(i); }
    }
    for (_, p, key) in &s.flying { crate::gunfx_hook::projectile_move(*key, p.pos, p.vel); }
    // RETICLE HOOK: SFortReticle reads heat * multiplier of the held weapon (after this frame's fire calls added their heat)
    if let Ok(mut r) = RETICLE_SPREAD.lock() { *r = held.map_or(0., |g| s.spread[g].heat * s.spread[g].mult); }
    // RECOIL HOOK: UpdateRecoil (vtable +0x760, RVA 0x936200) of the HELD firearm adds its step to the look delta; sim.begin_frame applies the delta to the view
    if let Some(g) = held {
        let mut d = [inp.pitch_delta, inp.yaw_delta, 0.];
        s.recoil[g].update(&recoil_stats(&s.guns[g].stats), dt, &mut d, s.now, 1., ATHENA_RECOIL);
        inp.pitch_delta = d[0]; inp.yaw_delta = d[1];
    }
}

/// RECOIL HOOK: `Weapon.UseAthenaRecoil` = 0 -> legacy `Recoil*` columns (`fn_weapons::recovered_recoil::USE_ATHENA_RECOIL_EXE_DEFAULT`).
/// Evidence: registered by RVA 0xafd10 as a `FAutoConsoleVariableRef` on the `.bss` int 0x14662c1a0 (zero-fill); the only other references are the reads in
/// AddRecoil (0x917930) and UpdateRecoil (0x936200), no writer in the exe; the cooked config has no `Weapon.UseAthenaRecoil` in `[SystemSettings]` or
/// `[ConsoleVariables]` and no DeviceProfile CVar (CookedIniVersion.txt L966-997, L1917-1936, L5587-5590). Only an online hotfix could set it (not covered).
/// This replaces the earlier `true` (2026-10-04), which assumed the then-unreadable engine config set it. See fn-weapons/RECOIL.md.
const ATHENA_RECOIL: bool = fn_weapons::recovered_recoil::USE_ATHENA_RECOIL_EXE_DEFAULT;
fn recoil_stats(s: &fn_hitreg::data::StatRow) -> RecoilStats {
    RecoilStats { vert: s.recoil_vert, vert_scale: s.recoil_vert_scale, vert_scale_gamepad: s.recoil_vert_scale_gamepad, vert_down_chance: s.vert_recoil_down_chance, horiz: s.recoil_horiz, horiz_scale: s.recoil_horiz_scale, horiz_scale_gamepad: s.recoil_horiz_scale_gamepad,
        interp_speed: s.recoil_interp_speed, recovery_interp_speed: s.recoil_recovery_interp_speed, recovery_delay: s.recoil_recovery_delay, recovery_fraction: s.recoil_recovery_fraction, downsights_mult: s.recoil_downsights_mult,
        athena_mag_min: s.athena_recoil_magnitude.0, athena_mag_max: s.athena_recoil_magnitude.1, athena_mag_scale: s.athena_recoil_magnitude_scale, athena_angle_min: s.athena_recoil_angle.0, athena_angle_max: s.athena_recoil_angle.1,
        athena_roll_min: s.athena_recoil_roll.0, athena_roll_max: s.athena_recoil_roll.1, athena_interp_speed: s.athena_recoil_interp_speed, athena_recovery_interp_speed: s.athena_recoil_recovery_interp_speed, athena_downsights_mult: s.athena_recoil_downsights_mult }
}

