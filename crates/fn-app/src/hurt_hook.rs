//! HURT HOOK (fn-app): what the 1.8 BR pawn does on the screen when it takes damage, and the FALL DAMAGE that currently feeds it.
//!
//! * [`on_frame`] (once per frame, from `App` after the arms update): drains `GameSim::fall` landings, applies `fn_game::fall_damage::fall_damage` to the pawn's
//!   shield / health (`Arms`), and plays the damage feedback for every landing that hurt.
//! * [`frame`] (from `particles::submit`): the live damage-direction lens effects as particle batches.
//! * [`apply_shake`] (from `scene::camera_for`): the `PlayerTakeDamage_CameraShake` offsets of the current frame.
//!
//! RECOVERED (cooked data + exe; addresses as in `D:\FortniteRef\decomp\named`):
//! * `PlayerPawn_Athena_Generic.OnDamagePlayEffects` (bytecode, event 848 -> ubergraph): `LatestDamageIsShieldDamage = HasAnyTags(DamageTags, GameplayCue.Damage.Shielded)`;
//!   `SpawnCameraLensEffectDirectional(Shielded ? ScreenEffect_PlayerShieldDamageDirection : ScreenEffect_PlayerHealthDamageDirection, InstigatedBy, HitInfo)`;
//!   `SpawnCameraLensEffect(None)`: the class literal is null, so the full-screen `B_PlayerHealthDamage_CameraLensEffect` (`P_Screen_HealthDamage_Effect`,
//!   `M_Screen_Damage`) is never spawned from this path (AFortPawn::SpawnCameraLensEffect 0x1408055a0 -> camera vfunc 0x6e0 with a null class) and its
//!   `PassParticle_Parameter` casts fail; then (local controller) `ClientPlayCameraShake(DamageCameraShake = PlayerTakeDamage_CameraShake, Scale 1)` and force feedback.
//! * `AFortPawn::SpawnCameraLensEffectDirectional` 0x140805640: with no instigator, `DamageInstigatorLocation = HitInfo.Location + HitInfo.Normal * vfunc 0x8c0()`.
//!   For a fall there is no instigator and no hit (the event's HitInfo is zeroed in AFortPawn vfunc 323 0x1407fa020 before the context fills it), so the location is
//!   the UE world origin (INFERRED: the context's hit fill for an environment effect was not traced; with no hit result it stays zero).
//! * `AFortEmitterCameraLensEffectDirectional` ctor 0x140572e40: `RotationParameterName = None`, `bRotateActor = 1`; `UpdateLocation` 0x1405a30d0: the base placement
//!   (`AEmitterCameraLensEffectBase::GetAttachedEmitterTransform`, UE4.16 ParticleComponents.cpp:7187: RelativeTransform = yaw 180, X 90 cm, X scaled by
//!   tan(BaseFOV 80 / 2) / tan(FOV / 2)), then `Roll += EyesViewPoint.Yaw - (DamageInstigatorLocation - PawnLocation).Rotation().Yaw`.
//! * the systems `P_Screen_HealthDamage_Effect_Direction` / `P_Screen_ShieldDamage_Effect_Direction` (two mesh emitters `S_Hud_Damage_Mesh` 1.4 s / `S_Hud_Damage_Mesh2`
//!   0.3 s, local space) run unchanged on `fn_particles` (exported to `<assets>/particles-hurt` by `fn-particles-export`).
//! * `M_Hud_Damage` (BLEND_Translucent, MSM_Unlit), from its compiled mesh-particle base-pass pixel shader (SM5 `TBasePassPS`, 628 bytes, `fn-dxbc material`):
//!   `rgb = max(ParticleColor.rgb + SelectionColor(0), 0)` with the height fog, `alpha = saturate(T_Damage_Direction_Indicator.r * ParticleColor.a)`; the texture
//!   only drives the opacity. The catalog `Look` heuristic (texture x colour) is replaced by exactly that ([`material`]).
//! * `PlayerTakeDamage_CameraShake` CDO: OscillationDuration 0.25, BlendIn 0, BlendOut 0.1, LocOscillation X (3, 10 Hz) Y (2, 15), RotOscillation Pitch (0.5, 20)
//!   Yaw (0.5, 25) Roll (0.5, 15); UE4.16 `UCameraShake::UpdateAndApplyCameraShake` (CameraShake.cpp:204) with `EOO_OffsetRandom` start phases, camera-local play space.
//!
//! INFERRED: the Shield variant is chosen when the fall took shield (`GameplayCue.Damage.Shielded` is assumed to accompany shield damage; its source was not traced);
//! max health = the port's 100 (`Arms::MAX_HEALTH`, standing in for `GetMaxHealth`, vfunc 0x960).
//! NOT PORTED: the third-person `HitDamageParticles` on the pawn mesh, force feedback, the SDPG_Foreground draw (the lens meshes are depth-tested like other
//! translucent particles, so a surface or the pawn itself within 90 cm of the camera can hide them). The OnDamagePlayEffects block plays no sound.
use crate::arms::{Arms, MAX_HEALTH};
use crate::particles::Gpu;
use fn_game::fall_damage::{fall_damage, FallHit};
use fn_game::GameSim;
use fn_particles::assets::Catalog;
use fn_particles::{Env, Instance, Transform};
use fn_render::particles::*;
use fn_render::{CameraParams, Renderer};
use glam::{Mat3, Vec3};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const P_HEALTH_DIRECTION: &str = "/Game/Effects/Fort_Effects/Camera_Effects/Effects/P_Screen_HealthDamage_Effect_Direction";
pub const P_SHIELD_DIRECTION: &str = "/Game/Effects/Fort_Effects/Camera_Effects/Effects/P_Screen_ShieldDamage_Effect_Direction";
const M_HUD_DAMAGE: &str = "/Game/Effects/Fort_Effects/Camera_Effects/M_Hud_Damage.M_Hud_Damage";
const T_INDICATOR_PNG: &str = "textures/T_Damage_Direction_Indicator.png";
/// `AEmitterCameraLensEffectBase` ctor (UE4.16 ParticleComponents.cpp:7167-7176): BaseFOV, RelativeTransform location X (cm), yaw 180.
pub const BASE_FOV: f32 = 80.;
pub const LENS_DIST_CM: f32 = 90.;

/// One `UCameraShake` oscillator: (amplitude, frequency).
#[derive(Clone, Copy, Debug)]
struct Osc { amp: f32, freq: f32 }
/// `PlayerTakeDamage_CameraShake` (cooked CDO).
const SHAKE_DURATION: f32 = 0.25;
const SHAKE_BLEND_OUT: f32 = 0.1;
const SHAKE_LOC: [Osc; 3] = [Osc { amp: 3., freq: 10. }, Osc { amp: 2., freq: 15. }, Osc { amp: 0., freq: 0. }];
const SHAKE_ROT: [Osc; 3] = [Osc { amp: 0.5, freq: 20. }, Osc { amp: 0.5, freq: 25. }, Osc { amp: 0.5, freq: 15. }]; // pitch, yaw, roll

#[derive(Clone, Debug, Default)]
struct Shake { remaining: f32, blending_out: bool, blend_out_t: f32, loc_phase: [f32; 3], rot_phase: [f32; 3], loc: [f32; 3], rot: [f32; 3] }
impl Shake {
    fn start(&mut self, rnd: &mut u64) {
        let mut r = || { *rnd = rnd.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); ((*rnd >> 40) as f32 / (1u64 << 24) as f32) * std::f32::consts::TAU };
        *self = Shake { remaining: SHAKE_DURATION, loc_phase: [r(), r(), r()], rot_phase: [r(), r(), r()], ..Default::default() };
    }
    /// `UpdateAndApplyCameraShake` (Alpha 1, ShakeScale 1): advances the phases and leaves this frame's offsets in `loc` (cm, camera X/Y/Z) / `rot` (deg, P/Y/R).
    fn update(&mut self, dt: f32) {
        self.loc = [0.; 3]; self.rot = [0.; 3];
        if self.remaining <= 0. { return; }
        self.remaining = (self.remaining - dt).max(0.);
        if self.blending_out { self.blend_out_t += dt; }
        let mut finished = self.remaining == 0.;
        if !finished && self.remaining < SHAKE_BLEND_OUT { self.blending_out = true; self.blend_out_t = SHAKE_BLEND_OUT - self.remaining; }
        if self.blending_out && self.blend_out_t > SHAKE_BLEND_OUT { self.blend_out_t = SHAKE_BLEND_OUT; finished = true; }
        if finished { self.remaining = 0.; return; }
        let w = if self.blending_out { 1. - self.blend_out_t / SHAKE_BLEND_OUT } else { 1. };
        let upd = |o: Osc, ph: &mut f32| if o.amp != 0. { *ph += dt * o.freq; o.amp * ph.sin() } else { 0. };
        for k in 0..3 { self.loc[k] = upd(SHAKE_LOC[k], &mut self.loc_phase[k]) * w; self.rot[k] = upd(SHAKE_ROT[k], &mut self.rot_phase[k]) * w; }
    }
}

struct Lens { inst: Instance, target_ue: Vec3 }
struct State { catalog: Option<Arc<Catalog>>, gpu: Gpu, live: Vec<Lens>, pawn_ue: Vec3, shake: Shake, rnd: u64, serial: u64 }
static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Loads `<root>/particles-hurt` (or `FN_HURT_FX_DIR`). Without it the damage still applies, only the lens effect is missing.
pub fn init(roots: &[PathBuf]) {
    let dir = fn_live::var_os("FN_HURT_FX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles-hurt")).find(|d| d.join("index.json").is_file()));
    let catalog = match dir.as_ref().map(|d| Catalog::load(d)) {
        Some(Ok(c)) if c.system(P_HEALTH_DIRECTION).is_some() => { eprintln!("[hurt] damage direction lens effects loaded from {}", dir.as_ref().unwrap().display()); Some(Arc::new(c)) }
        Some(Ok(_)) => { eprintln!("[hurt] P_Screen_HealthDamage_Effect_Direction is not in the particles-hurt catalog: no damage indicator"); None }
        Some(Err(e)) => { eprintln!("[hurt] {e}: no damage indicator"); None }
        None => { eprintln!("[hurt] no `particles-hurt` folder in the asset roots (fn-particles-export --out <assets>/particles-hurt P_Screen_HealthDamage_Effect_Direction P_Screen_ShieldDamage_Effect_Direction): no damage indicator"); None }
    };
    *STATE.lock().unwrap() = Some(State { catalog, gpu: Gpu::default(), live: vec![], pawn_ue: Vec3::ZERO, shake: Shake::default(), rnd: 0x5eed, serial: 0 });
}

/// Feet position of the pawn in particle (UE cm) space.
fn pawn_ue(sim: &GameSim) -> Vec3 { fn_particles::sim::game_to_ue([sim.x, sim.y - fn_game::EYE, sim.z]) }
/// The UE world origin in particle space (the port's game space is the UE world shifted by the landscape mapping; without one the game origin stands in).
fn world_origin_ue() -> Vec3 {
    match fn_terrain::world() { Some(w) => { let g = w.map.to_game(Vec3::ZERO); fn_particles::sim::game_to_ue([g.x, g.y, g.z]) } None => Vec3::ZERO }
}

/// Per frame: fall landings -> damage (shield first, see fall_damage.rs) -> damage feedback; advances the camera shake by `dt`.
pub fn on_frame(sim: &mut GameSim, arms: &mut Arms, dt: f32) {
    crate::water_hook::on_frame(sim, arms);
    let landings = sim.fall.take_landings();
    let mut guard = STATE.lock().unwrap();
    let st = guard.get_or_insert_with(|| State { catalog: None, gpu: Gpu::default(), live: vec![], pawn_ue: Vec3::ZERO, shake: Shake::default(), rnd: 0x5eed, serial: 0 });
    st.pawn_ue = pawn_ue(sim);
    for h_cm in landings {
        let hit: FallHit = fall_damage(h_cm, arms.health, arms.shield, MAX_HEALTH);
        if !hit.any() { if h_cm > 100. { eprintln!("[fall] landed after {:.0} cm: no damage (curve {:.3})", h_cm, hit.fraction); } continue; }
        let (h0, s0) = (arms.health, arms.shield);
        arms.shield = (arms.shield - hit.shield_damage).max(0.);
        arms.health = (arms.health - hit.health_damage).max(0.);
        eprintln!("[fall] landed after {:.0} cm: AthenaFallingDamage {:.4} x max health {MAX_HEALTH} = {:.2} damage -> shield {s0} -> {}, health {h0} -> {}", h_cm, hit.fraction, hit.damage, arms.shield, arms.health);
        crate::kill_feed_hook::local_damaged(fn_ui::screens::kill_feed::DeathCause::FallDamage, fn_ui::screens::kill_feed::Killer::Themself, arms.health);
        // OnDamagePlayEffects (local pawn): the direction lens effect + the damage camera shake
        st.shake.start(&mut st.rnd);
        if let Some(cat) = st.catalog.clone() {
            let sys = if hit.shield_damage > 0. { P_SHIELD_DIRECTION } else { P_HEALTH_DIRECTION };
            if let Some(sys) = cat.system(sys) {
                st.serial += 1;
                let inst = Instance::new(sys, Transform::identity(), 0x4855_5254 ^ st.serial, LENS_DIST_CM);
                st.live.push(Lens { inst, target_ue: world_origin_ue() });
            }
        }
    }
    st.shake.update(dt);
}

pub fn on_damage(shield_hit: bool, instigator_ue: Vec3) {
    let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
    let st = guard.get_or_insert_with(|| State { catalog: None, gpu: Gpu::default(), live: vec![], pawn_ue: Vec3::ZERO, shake: Shake::default(), rnd: 0x5eed, serial: 0 });
    st.shake.start(&mut st.rnd);
    if let Some(cat) = st.catalog.clone() {
        if let Some(sys) = cat.system(if shield_hit { P_SHIELD_DIRECTION } else { P_HEALTH_DIRECTION }) {
            st.serial += 1;
            let inst = Instance::new(sys, Transform::identity(), 0x4855_5254 ^ st.serial, LENS_DIST_CM);
            st.live.push(Lens { inst, target_ue: instigator_ue });
        }
    }
}

/// `FRotationMatrix(FRotator(pitch, yaw, roll))` (degrees): the X, Y, Z axes in UE space (Engine/Classes RotationTranslationMatrix.h).
pub fn ue_rotation_axes(pitch: f32, yaw: f32, roll: f32) -> [Vec3; 3] {
    let (sp, cp) = pitch.to_radians().sin_cos(); let (sy, cy) = yaw.to_radians().sin_cos(); let (sr, cr) = roll.to_radians().sin_cos();
    [Vec3::new(cp * cy, cp * sy, sp), Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp), Vec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp)]
}
/// UE (pitch, yaw) in degrees of a UE-space direction (`FVector::Rotation`).
fn rotation_of(v: Vec3) -> (f32, f32) { (v.z.atan2((v.x * v.x + v.y * v.y).sqrt()).to_degrees(), v.y.atan2(v.x).to_degrees()) }

/// `AFortEmitterCameraLensEffectDirectional::UpdateLocation` for a camera at `eye_ue` looking along `fwd_ue` with horizontal FOV `fov`, the pawn at `pawn_ue` and the
/// damage location `target_ue` (all UE cm): the actor transform of the lens effect.
pub fn lens_transform(eye_ue: Vec3, fwd_ue: Vec3, fov: f32, pawn_ue: Vec3, target_ue: Vec3) -> Transform {
    let (cam_pitch, cam_yaw) = rotation_of(fwd_ue);
    let scale = (BASE_FOV * 0.5).to_radians().tan() / (fov.clamp(1., 170.) * 0.5).to_radians().tan();
    let pos = eye_ue + fwd_ue.normalize_or_zero() * (LENS_DIST_CM * scale);
    // RelativeTransform yaw 180 composed with the camera rotation = FRotator(-pitch, yaw + 180, 0); bRotateActor: Roll += EyesYaw - DamageDirectionYaw
    let (_, dir_yaw) = rotation_of(target_ue - pawn_ue);
    let roll = cam_yaw - dir_yaw;
    let [x, y, z] = ue_rotation_axes(-cam_pitch, cam_yaw + 180., roll);
    Transform { pos, rot: Mat3::from_cols(x, y, z), scale: 1.0 }
}

/// The compiled `M_Hud_Damage` mesh-particle base pass (module docs): colour = particle colour, opacity = texture red x particle alpha.
fn material(tex: Option<ParticleTexId>) -> ParticleMaterialDesc {
    ParticleMaterialDesc { label: "M_Hud_Damage (DXBC)".into(), blend: fn_render::fx::FxBlend::Translucent, unlit: true, two_sided: true, texture: tex, opacity_channel: Some(0), value_channel: Some(0), ramp_mode: 2, ramp: [[1.; 3]; 3], opacity_exponent: 1., ..Default::default() }
}

/// The lens effects of this frame (ticked by `dt`, placed in front of `camera`).
pub fn frame(renderer: &mut Renderer, camera: &CameraParams, dt: f32) -> Option<ParticleFrame> {
    let mut guard = STATE.lock().ok()?;
    let st = guard.as_mut()?;
    if st.live.is_empty() { return None; }
    let cat = st.catalog.clone()?;
    crate::particles::preset_material(renderer, &cat, &mut st.gpu, M_HUD_DAMAGE, T_INDICATOR_PNG, true, material);
    let eye = [camera.eye.x, camera.eye.y, camera.eye.z];
    let eye_ue = fn_particles::sim::game_to_ue(eye);
    let fwd_ue = fn_particles::sim::game_to_ue([camera.forward.x, camera.forward.y, camera.forward.z]).normalize_or_zero();
    let env = Env::none();
    let mut frame = ParticleFrame::default();
    for l in st.live.iter_mut() {
        l.inst.xf = lens_transform(eye_ue, fwd_ue, camera.fov_deg, st.pawn_ue, l.target_ue);
        l.inst.tick(dt, &env);
        let mut draws = vec![]; l.inst.draw(eye, &mut draws);
        let sort = crate::particles::instance_sort(&l.inst, &cat, camera);
        for d in &draws { crate::particles::batches(renderer, &cat, &mut st.gpu, d, sort, &mut frame); }
    }
    st.live.retain(|l| !l.inst.finished());
    Some(frame)
}

/// Adds this frame's damage camera shake to `c` (camera-local location offset in cm, rotation offset P/Y/R in degrees applied in camera space).
pub fn apply_shake(c: &mut CameraParams) {
    let Ok(g) = STATE.lock() else { return };
    let Some(st) = g.as_ref() else { return };
    let (l, r) = (st.shake.loc, st.shake.rot);
    if l == [0.; 3] && r == [0.; 3] { return; }
    let f = c.forward.normalize_or_zero(); let u = c.up.normalize_or_zero(); let right = f.cross(u).normalize_or_zero();
    c.eye += (f * l[0] + right * l[1] + u * l[2]) * 0.01;
    // AnimRotMat * CamRot: pitch about the camera's right (up positive), yaw about its up (right positive), roll about its forward
    let q = glam::Quat::from_axis_angle(f, r[2].to_radians()) * glam::Quat::from_axis_angle(u, -r[1].to_radians()) * glam::Quat::from_axis_angle(right, r[0].to_radians());
    c.forward = q * f; c.up = q * u;
}

/// Live lens effects (tests / logs).
pub fn live() -> usize { STATE.lock().ok().and_then(|g| g.as_ref().map(|s| s.live.len())).unwrap_or(0) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn lens_sits_90cm_ahead_at_80_degrees_and_scales_with_fov() {
        let xf = lens_transform(Vec3::ZERO, Vec3::X, 80., Vec3::new(0., 0., -100.), Vec3::new(1000., 0., -100.));
        assert!((xf.pos - Vec3::new(90., 0., 0.)).length() < 1e-3);
        // the actor faces the camera: its X axis points back along -forward, Z stays up when the damage is straight ahead
        assert!((xf.rot.x_axis - Vec3::new(-1., 0., 0.)).length() < 1e-4 && (xf.rot.z_axis - Vec3::Z).length() < 1e-4, "{:?}", xf.rot);
        let xf = lens_transform(Vec3::ZERO, Vec3::X, 40., Vec3::ZERO, Vec3::X);
        assert!((xf.pos.x - 90. * 40f32.to_radians().tan() / 20f32.to_radians().tan()).abs() < 1e-2);
    }
    #[test] fn the_indicator_turns_toward_the_damage() {
        // mesh offset is +Z (above the crosshair) in actor space; a source 90 degrees to the right (UE +Y) rolls it to the screen's right (UE +Y seen from the camera)
        let xf = lens_transform(Vec3::ZERO, Vec3::X, 80., Vec3::ZERO, Vec3::new(0., 1000., 0.));
        let up_in_world = xf.rot * Vec3::Z;
        assert!(up_in_world.y > 0.99, "a source to the right puts the arc on the right: {up_in_world:?}");
        let xf = lens_transform(Vec3::ZERO, Vec3::X, 80., Vec3::ZERO, Vec3::new(-1000., 0., 0.));
        assert!((xf.rot * Vec3::Z).z < -0.99, "a source behind puts the arc below the crosshair");
    }
    #[test] fn shake_runs_0_25s_and_fades_over_the_last_0_1s() {
        let mut s = Shake::default(); let mut r = 1; s.start(&mut r);
        let mut t = 0.; let mut peak = 0f32;
        while t < 0.4 { s.update(1. / 120.); t += 1. / 120.; peak = peak.max(s.loc[0].abs()); if t > 0.26 { assert_eq!(s.loc, [0.; 3]); } }
        assert!(peak > 1. && peak <= 3.0001);
    }
}
