//! Athena third-person camera (FortCameraMode_ThirdPerson as configured by Athena_PlayerCameraModeMelee).
//!
//! DATA recovered from the original pak (reports/camera-modes.json, tools/camera-evidence.cjs): every number in
//! `BASE`, `MELEE` and `FEELERS` below is a serialized Blueprint class default, units cm (converted to metres on use).
//! The equipped pickaxe selects the Melee mode: B_Melee_Impact_Pickaxe_Athena.CameraBase3PClass.
//!
//! RECONSTRUCTION (NOT recovered; the native C++ UpdateView is not in the pak, so the algorithm below is the
//! standard Unreal third-person camera pipeline and every item marked ASSUMED may differ from Fortnite):
//!  * pivot = capsule centre, lagged per axis in view-yaw space by CameraOriginInterpSpeed (ASSUMED space + FInterpTo)
//!  * + ViewTargetSpaceViewOffset (crouch variant when crouched) in yaw space, interpolated at ViewTargetSpaceViewOffseInterpSpeed
//!  * + CameraSpaceViewOffset sampled by pitch (piecewise linear) and CameraSpaceForwardDistance along the view
//!  * collision: PenetrationAvoidanceFeelers (data) swept against everything that blocks ECC_Camera (the caller's `trace`); safe
//!    location, feeler rays, fractions and blend translated from the exe (vfunc_69 @1405020b0 -> @1404f85a0, see `Camera::update`)
//!  * ASSUMED: the camera target (actor location) stays at the standing capsule centre while crouched (UE shrinks the capsule\n//!    about its centre and Fortnite handles the mesh itself via CrouchMeshOffset), so the crouch drop is exactly the serialized\n//!    ViewTargetSpaceViewOffset change (70 -> 30 cm) played through its interpolation, with no instantaneous capsule pop.\n//!  * capsule radius / crouched half-height, pitch limits, near plane and aspect handling are native defaults (ASSUMED)
use glam::Vec3;

/// cm -> m (world units of this runtime are metres).
pub const CM: f32 = 0.01;
/// Serialized: PlayerPawn_Athena_Generic.CollisionCylinder.CapsuleHalfHeight = 75 cm.
pub const STAND_HALF_HEIGHT: f32 = 75. * CM;
/// ASSUMED: APlayerCameraManager default ViewPitchMin/Max.
pub const PITCH_MIN: f32 = -89.9;
pub const PITCH_MAX: f32 = 89.9;
/// CONFIG-VERIFIED: GNearClippingPlane 10 cm (decrypted BaseEngine.ini line 208 `NearClipPlane=10.0`, not overridden by DefaultEngine.ini). Original uses an infinite far plane; a finite one is used here.
pub const NEAR: f32 = 10. * CM;
pub const FAR: f32 = 3000.;
static FAR_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x453B_8000); // 3000.0f32 (checked by a test)
/// Current far clip distance in metres: `FAR` unless the View Distance setting (settings.rs) lowered it.
pub fn far_plane() -> f32 { f32::from_bits(FAR_BITS.load(std::sync::atomic::Ordering::Relaxed)) }
/// Sets the far plane, clamped to [10 m, FAR]. Used by the View Distance setting.
pub fn set_far_plane(metres: f32) { FAR_BITS.store(metres.clamp(10., FAR).to_bits(), std::sync::atomic::Ordering::Relaxed); }
/// ASSUMED: FOV is authored at 16:9 and ULocalPlayer's default AspectRatioAxisConstraint (MaintainYFOV) keeps the
/// vertical FOV when the window aspect changes (Hor+), so FOV 80 is exactly horizontal at 16:9 only.
pub const REFERENCE_ASPECT: f32 = 16. / 9.;
/// `UFortCameraMode_ThirdPerson` native ctor @1404d43b0 (no Athena BP override in reports/camera-modes.json):
/// `PenetrationBlendInTime` (+0xD4) = 0.1 s, `PenetrationBlendOutTime` (+0xD8) = 0.15 s, read by the penetration avoidance @1404f85a0.
pub const PENETRATION_BLEND_IN_TIME: f32 = 0.1;
pub const PENETRATION_BLEND_OUT_TIME: f32 = 0.15;
/// @1404f85a0: the blocked fraction is applied to the view only when it is below DAT_1440c5e20 = 0.99999.
const APPLY_BELOW: f32 = 0.99999;
/// Pawn capsule radius used for the penetration safe location (vfunc_69 @1405020b0 queries the root capsule's collision):
/// `AFortPawn::AFortPawn` @1407c9ff0 writes CapsuleComponent +0x694 = 42.0; no pawn Blueprint overrides it (= `fn_movement::params::CAPSULE_RADIUS`).
pub const CAPSULE_RADIUS: f32 = 42. * CM;
/// Crouched capsule half height (`fn_movement::params::CROUCHED_HALF_HEIGHT`, AFortPlayerPawnAthena ctor 0x140413f00 writes 50.0 to CMC +0x21c). UE's Crouch shrinks the capsule and
/// moves the actor (capsule centre) down by the difference, so the live capsule keeps its bottom at the feet.
pub const CROUCHED_HALF_HEIGHT: f32 = 50. * CM;
/// The live pawn capsule (centre = actor location, half height) for feet at `feet`.
/// UE4.16 `UCapsuleComponent::SetCapsuleSize` clamps the half height to >= the radius, which leaves the crouched 50 cm capsule unclamped.
pub fn live_capsule(feet: Vec3, crouched: bool) -> (Vec3, f32) { let hh = if crouched { CROUCHED_HALF_HEIGHT.max(CAPSULE_RADIUS) } else { STAND_HALF_HEIGHT }; (feet + Vec3::Y * hh, hh) }
/// Serialized: values >= this are "no smoothing" sentinels (1e11 on the lateral axis).
const INSTANT_SPEED: f32 = 1.0e6;

#[derive(Clone, Copy, Debug)]
pub struct Sample { pub pitch: f32, pub offset: [f32; 3] }
#[derive(Clone, Copy, Debug)]
pub struct Feeler { pub adjust_pitch: f32, pub adjust_yaw: f32, pub world_weight: f32, pub pawn_weight: f32, pub extent: f32, pub trace_interval: i32 }

#[derive(Clone, Copy, Debug)]
pub struct Mode {
    pub name: &'static str,
    pub fov: f32,
    pub forward_distance: f32,
    pub origin_interp: [f32; 3],
    pub vt_offset: &'static [Sample],
    pub vt_offset_crouch: &'static [Sample],
    pub vt_interp: [f32; 3],
    pub cs_offset: &'static [Sample],
    pub push_out: f32,
    pub hide_pawn_pct: f32,
    pub transition_time: f32,
    pub feelers: &'static [Feeler],
}

// Athena_PlayerCameraModeBase.PenetrationAvoidanceFeelers (inherited by Melee: not overridden).
pub const FEELERS: [Feeler; 5] = [
    Feeler { adjust_pitch: 0., adjust_yaw: 0., world_weight: 0.5, pawn_weight: 0.5, extent: 8., trace_interval: 0 },
    Feeler { adjust_pitch: 0., adjust_yaw: 1., world_weight: 0.4, pawn_weight: 0.4, extent: 0., trace_interval: 3 },
    Feeler { adjust_pitch: 0., adjust_yaw: -1., world_weight: 0.4, pawn_weight: 0.4, extent: 0., trace_interval: 3 },
    Feeler { adjust_pitch: 1., adjust_yaw: 0., world_weight: 0.4, pawn_weight: 0.4, extent: 0., trace_interval: 4 },
    Feeler { adjust_pitch: -1., adjust_yaw: 0., world_weight: 0.4, pawn_weight: 0.4, extent: 0., trace_interval: 4 },
];

/// Athena_PlayerCameraModeBase_C defaults.
pub const BASE: Mode = Mode {
    name: "Athena_PlayerCameraModeBase", fov: 80., forward_distance: -280.,
    origin_interp: [12., 1.0e11, 60.],
    vt_offset: &[Sample { pitch: 0., offset: [0., 45., 75.] }],
    vt_offset_crouch: &[Sample { pitch: 0., offset: [0., 45., 40.] }],
    vt_interp: [10., 10., 10.],
    cs_offset: &[Sample { pitch: 0., offset: [0., 0., 0.] }, Sample { pitch: 90., offset: [-10., 0., 25.] }, Sample { pitch: -90., offset: [10., 0., 25.] }],
    push_out: 0.2, hide_pawn_pct: 0., transition_time: 0.45, feelers: &FEELERS,
};

/// Athena_PlayerCameraModeMelee_C (child of Base): the mode the pickaxe selects.
pub const MELEE: Mode = Mode {
    name: "Athena_PlayerCameraModeMelee", fov: 80., forward_distance: -280.,
    origin_interp: [30., 1.0e11, 60.],
    vt_offset: &[Sample { pitch: 0., offset: [38., 31., 70.] }],
    vt_offset_crouch: &[Sample { pitch: 0., offset: [38., 31., 30.] }],
    vt_interp: [8., 8., 8.],
    cs_offset: &[Sample { pitch: 0., offset: [0., 0., 0.] }, Sample { pitch: 90., offset: [30., 0., 100.] }, Sample { pitch: -90., offset: [50., 0., -20.] }],
    push_out: 0.2, hide_pawn_pct: 0.15, transition_time: 0.45, feelers: &FEELERS,
};

pub const DBNO: Mode = Mode {
    name: "Athena_PlayerCamera_DBNO",
    vt_offset: &[Sample { pitch: 0., offset: [0., 10., 15.] }],
    transition_time: 2.0,
    ..BASE
};

pub fn clamp_pitch(pitch: f32) -> f32 { pitch.clamp(PITCH_MIN, PITCH_MAX) }

/// FInterpTo-style step (ASSUMED shape): moves `speed*dt` of the remaining distance, never overshoots.
pub fn interp_to(current: f32, target: f32, dt: f32, speed: f32) -> f32 {
    if speed <= 0. || speed >= INSTANT_SPEED { return target; }
    current + (target - current) * (dt * speed).clamp(0., 1.)
}

/// Piecewise-linear lookup by pitch over unsorted samples; constant for one sample, clamped outside the range.
pub fn sample_offset(samples: &[Sample], pitch: f32) -> Vec3 {
    let at = |s: &Sample| Vec3::from(s.offset);
    if samples.len() == 1 { return at(&samples[0]); }
    let mut sorted: Vec<&Sample> = samples.iter().collect();
    sorted.sort_by(|a, b| a.pitch.partial_cmp(&b.pitch).unwrap());
    if pitch <= sorted[0].pitch { return at(sorted[0]); }
    for pair in sorted.windows(2) {
        if pitch <= pair[1].pitch {
            let t = (pitch - pair[0].pitch) / (pair[1].pitch - pair[0].pitch);
            return at(pair[0]).lerp(at(pair[1]), t);
        }
    }
    at(sorted[sorted.len() - 1])
}

/// Frustum half-extents at the near plane (right, top) for a window: horizontal FOV defined at REFERENCE_ASPECT,
/// vertical FOV preserved for other aspects (see REFERENCE_ASPECT).
pub fn frustum(fov_deg: f32, width: f32, height: f32, near: f32) -> (f64, f64) {
    let aspect = (width.max(1.) / height.max(1.)) as f64;
    let top = ((fov_deg as f64) * 0.5).to_radians().tan() / REFERENCE_ASPECT as f64 * near as f64;
    (top * aspect, top)
}

/// View-yaw basis (yaw 0 faces -Z; right = +X at yaw 0), matching main.rs `horizontal_step`.
pub fn yaw_basis(yaw_deg: f32) -> (Vec3, Vec3) {
    let a = yaw_deg.to_radians();
    (Vec3::new(a.sin(), 0., -a.cos()), Vec3::new(a.cos(), 0., a.sin()))
}
/// Full camera basis (forward, right, up) for control yaw/pitch, roll 0.
pub fn view_basis(yaw_deg: f32, pitch_deg: f32) -> (Vec3, Vec3, Vec3) {
    let (fh, right) = yaw_basis(yaw_deg);
    let p = pitch_deg.to_radians();
    let forward = fh * p.cos() + Vec3::Y * p.sin();
    (forward, right, right.cross(forward))
}

/// Sphere sweep against the ground: the installed landscape (`fn_terrain::world()`, TERRAIN; the landscape blocks ECC_Camera) or the baseplate top surface (y = 0).
/// Returns the hit fraction in [0,1] along from->to.
pub fn trace_ground(from: Vec3, to: Vec3, radius: f32) -> Option<f32> { trace_ground_on(fn_terrain::world().as_deref(), from, to, radius) }
/// [`trace_ground`] against an explicit landscape (`None` = the baseplate). Where a hole of the landscape is under an end of the sweep the plane y = 0 stands in (CHOSEN).
pub fn trace_ground_on(terrain: Option<&fn_terrain::World>, from: Vec3, to: Vec3, radius: f32) -> Option<f32> {
    let Some(w) = terrain else { return trace_plane(from, to, radius) };
    let t = w.sweep_sphere(from, to, radius);
    if w.ground_y(from.x, from.z).is_some() && w.ground_y(to.x, to.z).is_some() { return t; }
    match (t, trace_plane(from, to, radius)) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) }
}
fn trace_plane(from: Vec3, to: Vec3, radius: f32) -> Option<f32> {
    if from.y < radius { return Some(0.); }
    if to.y >= radius { return None; }
    Some((from.y - radius) / (from.y - to.y))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Input { pub feet: Vec3, pub crouched: bool, pub yaw_deg: f32, pub pitch_deg: f32, pub dt: f32 }

#[derive(Clone, Copy, Debug, Default)]
pub struct View {
    pub eye: Vec3, pub forward: Vec3, pub right: Vec3, pub up: Vec3,
    pub yaw_deg: f32, pub pitch_deg: f32,
    /// Where the boom starts / where it wants to end, and the fraction of that segment actually kept.
    pub safe: Vec3, pub desired: Vec3, pub blocked_pct: f32,
    pub hide_pawn: bool,
}

#[derive(Clone, Debug)]
pub struct Camera {
    origin: Vec3, vt: Vec3, penetration: f32, initialised: bool,
    /// `FPenetrationAvoidanceFeeler::FramesUntilNextTrace` (+0x1C of each 0x20-byte feeler).
    frames_until: [i32; 8],
    /// Camera mode +0x48 bit 0: set when the mode (re)starts, cleared at the end of vfunc_69 @1405020b0; while set the
    /// penetration avoidance @1404f85a0 takes this frame's blocked fraction without blending.
    reset_blend: bool,
    /// Crouch state of the last tick: `compose` takes the live capsule for the safe location from it.
    crouched: bool,
}
impl Default for Camera { fn default() -> Self { Camera { origin: Vec3::ZERO, vt: Vec3::ZERO, penetration: 1., initialised: false, frames_until: [0; 8], reset_blend: true, crouched: false } } }

/// Penetration-avoidance safe location (`UFortCameraMode_ThirdPerson` vfunc_69 @1405020b0, just before the call to @1404f85a0):
/// `L` = the point of the aim line (desired view location along the view direction) closest to the capsule centre; the safe location
/// is `GetSquaredDistanceToCollision(L)`'s closest point on the pawn capsule (UE4.16 `FBodyInstance::GetSquaredDistanceToBody`: `L`
/// itself when inside), pushed into the capsule by `Feeler[0].Extent + CollisionPushOutDistance` along `normalize(closest - L)`
/// (a zero vector when `L` was inside). The Z clamp the exe computes before that query is overwritten by its result (dead).
pub fn safe_location(mode: &Mode, feet: Vec3, crouched: bool, desired: Vec3, forward: Vec3) -> Vec3 {
    // the LIVE capsule: the line point is taken toward the actor (root component) location and the closest point is on the current
    // capsule, so a crouched pawn uses the 42 cm half height (the 40 cm CrouchedHalfHeight clamped up to the capsule radius) centred 42 cm above the feet
    let (center, hh) = live_capsule(feet, crouched);
    let l = desired + forward * (center - desired).dot(forward);
    let seg_h = (hh - CAPSULE_RADIUS).max(0.);
    let axis = Vec3::new(center.x, (l.y).clamp(center.y - seg_h, center.y + seg_h), center.z);
    let d = l - axis;
    if d.length() <= CAPSULE_RADIUS { return l; }
    let closest = axis + d.normalize() * CAPSULE_RADIUS;
    let push_in = mode.feelers.first().map_or(0., |f| f.extent) * CM + mode.push_out * CM;
    let n = closest - l;
    closest + if n.length_squared() >= 1e-8 { n.normalize() * push_in } else { Vec3::ZERO }
}

/// `FVector::RotateAngleAxis` (UE, degrees) of `v` about the unit `axis` (UE left-handed X fwd / Y right / Z up formula). Here applied
/// in the port frame through the ray's (forward, right, up) basis: a +yaw about `up` turns forward toward `right`, a +pitch about
/// `right` turns forward toward `-up` (that is what UE's formula does for the Y axis), so the exe's feeler rays are
/// `forward cos(yaw) cos(pitch) + right sin(yaw) - up cos(yaw) sin(pitch)`.
fn feeler_dir(forward: Vec3, right: Vec3, up: Vec3, adjust_yaw_deg: f32, adjust_pitch_deg: f32) -> Vec3 {
    let (sy, cy) = adjust_yaw_deg.to_radians().sin_cos();
    let (sp, cp) = adjust_pitch_deg.to_radians().sin_cos();
    forward * (cy * cp) + right * sy - up * (cy * sp)
}

impl Camera {
    pub fn reset(&mut self) { *self = Camera::default(); }
    pub fn is_initialised(&self) -> bool { self.initialised }
    /// Current (interpolated) view-target-space offset in cm; exposed for tests/diagnostics.
    pub fn view_target_offset_cm(&self) -> Vec3 { self.vt }

    pub fn update(&mut self, mode: &Mode, input: Input, trace: impl Fn(Vec3, Vec3, f32) -> Option<f32>) -> View {
        let dt = input.dt.clamp(0., 0.1);
        let pitch = clamp_pitch(input.pitch_deg);
        let half = STAND_HALF_HEIGHT;
        let center = input.feet + Vec3::Y * half;
        let (fh, right_h) = yaw_basis(input.yaw_deg);
        let vt_target = sample_offset(if input.crouched { mode.vt_offset_crouch } else { mode.vt_offset }, pitch);
        // Teleport / respawn: snap instead of gliding across the map.
        if !self.initialised || (self.origin - center).length() > 3. {
            self.origin = center; self.vt = vt_target; self.penetration = 1.; self.initialised = true;
            self.frames_until = [0; 8]; self.reset_blend = true;
        }
        // Pivot lag, per axis in yaw space (X forward, Y right, Z up).
        let d = center - self.origin;
        let k = |s: f32| if s >= INSTANT_SPEED { 1. } else { (dt * s).clamp(0., 1.) };
        self.origin += fh * d.dot(fh) * k(mode.origin_interp[0]) + right_h * d.dot(right_h) * k(mode.origin_interp[1]) + Vec3::Y * d.y * k(mode.origin_interp[2]);
        // Shoulder / crouch offset interpolation (cm).
        self.vt = Vec3::new(
            interp_to(self.vt.x, vt_target.x, dt, mode.vt_interp[0]),
            interp_to(self.vt.y, vt_target.y, dt, mode.vt_interp[1]),
            interp_to(self.vt.z, vt_target.z, dt, mode.vt_interp[2]));
        let pivot = self.origin + (fh * self.vt.x + right_h * self.vt.y + Vec3::Y * self.vt.z) * CM;
        let (forward, right, up) = view_basis(input.yaw_deg, pitch);
        let cs = sample_offset(mode.cs_offset, pitch);
        let desired = pivot + (forward * (cs.x + mode.forward_distance) + right * cs.y + up * cs.z) * CM;
        let safe = safe_location(mode, input.feet, input.crouched, desired, forward);

        let ray = desired - safe;
        let len = ray.length();
        let mut blocked = 1.;
        if len > 1e-4 {
            // `UFortCameraMode_ThirdPerson` penetration avoidance @1404f85a0 ("CameraPen"), 1:1:
            // * each feeler whose FramesUntilNextTrace < 1 sphere-sweeps (SweepSingleByChannel, ECC_Camera = 4, sphere of Feeler.Extent)
            //   from the safe location to safe + ray rotated by the feeler's AdjustmentRot (yaw about the ray frame's Z, then pitch about
            //   its Y, `FVector::RotateAngleAxis`), then sets FramesUntilNextTrace = TraceInterval (0 again after a hit); a skipped
            //   feeler only decrements its counter and contributes nothing;
            // * a hit gives (|Hit.Location - safe| - CollisionPushOutDistance) / |target - safe|, min'd into the frame's fraction
            //   (start 1). WorldWeight / PawnWeight do not scale it (the decompile computes nothing from them);
            // * hard = the fraction after feeler 0, soft = after the last traced other feeler (both start at the current value).
            let dir = ray / len;
            let base_yaw = dir.x.atan2(-dir.z).to_degrees();
            let base_pitch = dir.y.clamp(-1., 1.).asin().to_degrees();
            let (rf, rr, ru) = view_basis(base_yaw, base_pitch);
            let cur = self.penetration;
            let (mut this_frame, mut hard, mut soft) = (1f32, cur, cur);
            for (i, f) in mode.feelers.iter().enumerate().take(8) {
                if self.frames_until[i] < 1 {
                    let target = if i == 0 { desired } else { safe + feeler_dir(rf, rr, ru, f.adjust_yaw, f.adjust_pitch) * len };
                    self.frames_until[i] = f.trace_interval;
                    if let Some(t) = trace(safe, target, f.extent * CM) {
                        let seg = target - safe;
                        let pct = ((seg * t).length() - mode.push_out * CM) / seg.length().max(1e-6);
                        this_frame = this_frame.min(pct);
                        self.frames_until[i] = 0;
                    }
                    if i == 0 { hard = this_frame; } else { soft = this_frame; }
                } else { self.frames_until[i] -= 1; }
            }
            // blend (@1404f85a0 tail): first frame of the mode -> take it; a clearer view blends out over PenetrationBlendOutTime; a
            // view more blocked than the hard (feeler 0) fraction snaps to it; otherwise a lower soft fraction blends in over
            // PenetrationBlendInTime (each jumps when the time <= dt). Clamped to [0, 1].
            let mut p = cur;
            if self.reset_blend { p = this_frame; }
            else if this_frame > p { p = if PENETRATION_BLEND_OUT_TIME <= dt { this_frame } else { p + (this_frame - p) * (dt / PENETRATION_BLEND_OUT_TIME) }; }
            else if p > hard { p = hard; }
            else if soft < p { p = if PENETRATION_BLEND_IN_TIME <= dt { soft } else { p - (p - soft) * (dt / PENETRATION_BLEND_IN_TIME) }; }
            self.penetration = p.clamp(0., 1.);
            blocked = if self.penetration < APPLY_BELOW { self.penetration } else { 1. };
        }
        self.reset_blend = false;
        self.crouched = input.crouched;
        let eye = safe + ray * blocked;
        View { eye, forward, right, up, yaw_deg: input.yaw_deg, pitch_deg: pitch, safe, desired, blocked_pct: blocked, hide_pawn: blocked < mode.hide_pawn_pct }
    }

    // ---- fixed-step split (src/sim_tick.rs): `advance` = the smoothing / collision state at the simulation tick, `compose` = the per-frame view ----
    /// The smoothed state that only changes at a simulation tick (pivot lag, shoulder/crouch offset, boom penetration blend).
    pub fn state(&self) -> CamState { CamState { origin: self.origin, vt: self.vt, penetration: self.penetration, crouched: self.crouched } }
    /// One simulation tick of the camera (origin smoothing, offset interpolation, feeler traces, penetration blend); the view of this tick is
    /// discarded: the game composes the visible view every frame with [`Camera::compose`].
    pub fn advance(&mut self, mode: &Mode, input: Input, trace: impl Fn(Vec3, Vec3, f32) -> Option<f32>) { let _ = self.update(mode, input, trace); }
    /// The view for the CURRENT yaw/pitch (mouse look is input, applied every frame) and an interpolated `feet`/[`CamState`]. Pure: with the state
    /// and feet of the last `update` it returns exactly that update's view (the boom is additionally hard-clipped against the main 8 cm probe of
    /// the ray it actually uses, so a yaw/pitch change between two ticks can never put the eye inside geometry).
    pub fn compose(mode: &Mode, st: &CamState, feet: Vec3, yaw_deg: f32, pitch_deg: f32, trace: impl Fn(Vec3, Vec3, f32) -> Option<f32>) -> View {
        let pitch = clamp_pitch(pitch_deg);
        let half = STAND_HALF_HEIGHT;
        let center = feet + Vec3::Y * half;
        let (fh, right_h) = yaw_basis(yaw_deg);
        let pivot = st.origin + (fh * st.vt.x + right_h * st.vt.y + Vec3::Y * st.vt.z) * CM;
        let (forward, right, up) = view_basis(yaw_deg, pitch);
        let cs = sample_offset(mode.cs_offset, pitch);
        let desired = pivot + (forward * (cs.x + mode.forward_distance) + right * cs.y + up * cs.z) * CM;
        let safe = safe_location(mode, feet, st.crouched, desired, forward);
        let ray = desired - safe;
        let len = ray.length();
        let mut blocked = 1.;
        if len > 1e-4 {
            // PORT (sub-tick view): the exe runs @1404f85a0 every frame; here the ticked fraction is additionally clipped by feeler 0
            // of THIS frame's ray (same formula as `update`), so a yaw/pitch change between ticks never puts the eye inside geometry.
            let f = &mode.feelers[0];
            let hard = match trace(safe, desired, f.extent * CM) { Some(t) => ((t * len - mode.push_out * CM) / len.max(1e-6)).clamp(0., 1.), None => 1. };
            let p = st.penetration.min(hard);
            blocked = if p < APPLY_BELOW { p } else { 1. };
        }
        View { eye: safe + ray * blocked, forward, right, up, yaw_deg, pitch_deg: pitch, safe, desired, blocked_pct: blocked, hide_pawn: blocked < mode.hide_pawn_pct }
    }
}

/// Camera smoothing state that changes once per simulation tick (see [`Camera::state`]); the renderer blends two of them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CamState { pub origin: Vec3, pub vt: Vec3, pub penetration: f32, pub crouched: bool }
impl CamState {
    pub fn lerp(a: &CamState, b: &CamState, t: f32) -> CamState { CamState { origin: a.origin.lerp(b.origin, t), vt: a.vt.lerp(b.vt, t), penetration: a.penetration + (b.penetration - a.penetration) * t, crouched: b.crouched } }
    pub fn follow(mode: &Mode, prev: Option<CamState>, ticked: &CamState, feet: Vec3, yaw_deg: f32, pitch_deg: f32, dt: f32) -> CamState {
        let dt = dt.clamp(0., 0.1);
        let center = feet + Vec3::Y * STAND_HALF_HEIGHT;
        let Some(mut s) = prev.filter(|p| (p.origin - center).length() <= 3.) else { return *ticked };
        let (fh, right_h) = yaw_basis(yaw_deg);
        let vt_target = sample_offset(if ticked.crouched { mode.vt_offset_crouch } else { mode.vt_offset }, clamp_pitch(pitch_deg));
        let d = center - s.origin;
        let k = |sp: f32| if sp >= INSTANT_SPEED { 1. } else { (dt * sp).clamp(0., 1.) };
        s.origin += fh * d.dot(fh) * k(mode.origin_interp[0]) + right_h * d.dot(right_h) * k(mode.origin_interp[1]) + Vec3::Y * d.y * k(mode.origin_interp[2]);
        s.vt = Vec3::new(interp_to(s.vt.x, vt_target.x, dt, mode.vt_interp[0]), interp_to(s.vt.y, vt_target.y, dt, mode.vt_interp[1]), interp_to(s.vt.z, vt_target.z, dt, mode.vt_interp[2]));
        s.penetration = ticked.penetration; s.crouched = ticked.crouched;
        s
    }
}
pub fn frame_follow_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| fn_live::var("FN_CAM_FRAME_FOLLOW").map_or(true, |v| v.trim() != "0"))
}

#[cfg(test)]
mod tests {
    use super::*;
    const EPS: f32 = 1e-4;
    fn near(a: Vec3, b: Vec3) { assert!((a - b).length() < EPS, "{a:?} != {b:?}"); }
    fn settle(c: &mut Camera, mode: &Mode, feet: Vec3, crouched: bool, yaw: f32, pitch: f32, seconds: f32) -> View {
        let mut v = View::default();
        for _ in 0..(seconds * 120.) as usize { v = c.update(mode, Input { feet, crouched, yaw_deg: yaw, pitch_deg: pitch, dt: 1. / 120. }, trace_ground); }
        v
    }

    // ---- asset-backed: constants must equal the extracted report (reports/camera-modes.json) ----
    fn report() -> serde_json::Value { serde_json::from_str(&fn_testdata::report_text("camera-modes.json")).unwrap() }
    fn v3(v: &serde_json::Value) -> [f32; 3] { [v[0].as_f64().unwrap() as f32, v[1].as_f64().unwrap() as f32, v[2].as_f64().unwrap() as f32] }
    fn check_samples(report: &serde_json::Value, code: &[Sample]) {
        let items = report.as_array().unwrap();
        assert_eq!(items.len(), code.len());
        for (r, c) in items.iter().zip(code) {
            assert_eq!(r["pitchAngle"].as_f64().unwrap() as f32, c.pitch);
            assert_eq!(v3(&r["viewOffsetCm"]), c.offset);
        }
    }
    fn check_mode(name: &str, m: &Mode) {
        let r = report();
        let e = &r["modes"][name]["effective"];
        assert_eq!(e["FOV_horizontalDeg"].as_f64().unwrap() as f32, m.fov);
        assert_eq!(e["CameraSpaceForwardDistanceCm"].as_f64().unwrap() as f32, m.forward_distance);
        let o = v3(&e["CameraOriginInterpSpeed_XYZ"]);
        assert_eq!([o[0], o[2]], [m.origin_interp[0], m.origin_interp[2]]);
        assert!(o[1] >= INSTANT_SPEED && m.origin_interp[1] >= INSTANT_SPEED);
        assert_eq!(v3(&e["ViewTargetSpaceViewOffsetInterpSpeed_XYZ"]), m.vt_interp);
        check_samples(&e["ViewTargetSpaceViewOffset"], m.vt_offset);
        check_samples(&e["ViewTargetSpaceViewOffset_Crouch"], m.vt_offset_crouch);
        check_samples(&e["CameraSpaceViewOffset"], m.cs_offset);
        assert!((e["CollisionPushOutDistanceCm"].as_f64().unwrap() as f32 - m.push_out).abs() < 1e-6);
        assert!((e["HidePawnPenetrationPercent"].as_f64().unwrap_or(0.) as f32 - m.hide_pawn_pct).abs() < 1e-6);
        assert!((e["TransitionTime"].as_f64().unwrap() as f32 - m.transition_time).abs() < 1e-6);
        let feelers = e["PenetrationAvoidanceFeelers"].as_array().unwrap();
        assert_eq!(feelers.len(), m.feelers.len());
        for (r, f) in feelers.iter().zip(m.feelers) {
            let rot = v3(&r["adjustmentRot_PitchYawRoll"]);
            assert_eq!((rot[0], rot[1], rot[2]), (f.adjust_pitch, f.adjust_yaw, 0.));
            assert!((r["worldWeight"].as_f64().unwrap() as f32 - f.world_weight).abs() < 1e-6);
            assert!((r["pawnWeight"].as_f64().unwrap() as f32 - f.pawn_weight).abs() < 1e-6);
            assert_eq!(r["extentCm"].as_f64().unwrap() as f32, f.extent);
            assert_eq!(r["traceInterval"].as_i64().unwrap() as i32, f.trace_interval);
        }
    }
    #[test] fn melee_constants_match_extracted_asset_report() { check_mode("Athena_PlayerCameraModeMelee", &MELEE); }
    #[test] fn base_constants_match_extracted_asset_report() { check_mode("Athena_PlayerCameraModeBase", &BASE); }
    #[test] fn pickaxe_selects_melee_camera_and_capsule_matches_report() {
        let r = report();
        assert_eq!(r["pickaxeCameraLink"]["CameraBase3PClass"], "import:Athena_PlayerCameraModeMelee_C");
        // 1.8: the WID -> weapon actor link is a soft-path string (WeaponActorClass), not a name-table entry (1.10 layout).
        assert_eq!(r["pickaxeCameraLink"]["widReferencesImpactBlueprint"], true);
        assert_eq!(r["pickaxeCameraLink"]["widWeaponActorClass"],
            "/Game/Weapons/FORT_Melee/Blueprints/Impact/B_Melee_Impact_Pickaxe_Athena.B_Melee_Impact_Pickaxe_Athena_C");
        // Second, independent route: the camera manager's "Athena" prototype replaces the default 3P camera with Melee.
        assert_eq!(r["cameraManager"]["Base3PClass"], "import:Default3PCamera_C");
        let athena = r["cameraManager"]["prototypes"].as_array().unwrap().iter().find(|p| p["name"] == "Athena").unwrap();
        assert!(athena["overrides"].as_array().unwrap().iter()
            .any(|o| o == "import:Default3PCamera_C -> import:Athena_PlayerCameraModeMelee_C"));
        assert_eq!(r["weaponCameraSelection"]["B_Melee_Impact_Pickaxe_Athena"]["CameraBase3PClass"], "import:Athena_PlayerCameraModeMelee_C");
        assert_eq!(r["pawn"]["capsuleHalfHeightCm"].as_f64().unwrap() as f32 * CM, STAND_HALF_HEIGHT);
        assert_eq!(r["modes"]["Athena_PlayerCameraModeMelee"]["parent"], "Athena_PlayerCameraModeBase");
    }

    // ---- math ----
    #[test] fn sample_offset_is_piecewise_linear_and_clamped() {
        near(sample_offset(MELEE.cs_offset, 0.), Vec3::ZERO);
        near(sample_offset(MELEE.cs_offset, 45.), Vec3::new(15., 0., 50.));
        near(sample_offset(MELEE.cs_offset, -45.), Vec3::new(25., 0., -10.));
        near(sample_offset(MELEE.cs_offset, 90.), Vec3::new(30., 0., 100.));
        near(sample_offset(MELEE.cs_offset, 200.), Vec3::new(30., 0., 100.));
        near(sample_offset(MELEE.cs_offset, -200.), Vec3::new(50., 0., -20.));
        near(sample_offset(MELEE.vt_offset, 77.), Vec3::new(38., 31., 70.));
    }
    #[test] fn standing_level_camera_is_over_right_shoulder_280cm_behind() {
        let mut c = Camera::default();
        let v = settle(&mut c, &MELEE, Vec3::ZERO, false, 0., 0., 2.);
        // capsule centre y=0.75; pivot +38 fwd(-z), +31 right(+x), +70 up; boom 280 back along view.
        near(v.eye, Vec3::new(0.31, 0.75 + 0.70, -0.38 + 2.80));
        assert!((v.blocked_pct - 1.).abs() < 1e-6 && !v.hide_pawn);
    }
    #[test] fn offsets_rotate_with_yaw() {
        let mut c = Camera::default();
        let v = settle(&mut c, &MELEE, Vec3::ZERO, false, 90., 0., 2.);
        // facing +X: forward=+x, right=+z. eye = pivot(+0.38x,+0.31z) - 2.8 forward.
        near(v.eye, Vec3::new(0.38 - 2.80, 1.45, 0.31));
    }
    #[test] fn pitch_changes_boom_per_camera_space_samples() {
        let mut c = Camera::default();
        let v = settle(&mut c, &MELEE, Vec3::new(0., 5., 0.), false, 0., -45., 2.);
        let (f, r, u) = view_basis(0., -45.);
        let pivot = Vec3::new(0.31, 5. + 0.75 + 0.70, -0.38);
        near(v.eye, pivot + (f * (25. - 280.) + r * 0. + u * -10.) * CM);
    }
    #[test] fn frustum_is_horizontal_80_at_16x9_and_keeps_vertical_fov_elsewhere() {
        let (r, t) = frustum(80., 1920., 1080., NEAR);
        assert!((r - 40f64.to_radians().tan() * NEAR as f64).abs() < 1e-7);
        assert!((r / t - 16. / 9.).abs() < 1e-6);
        let (r43, t43) = frustum(80., 1024., 768., NEAR);
        let (r219, t219) = frustum(80., 2560., 1080., NEAR);
        assert!((t - t43).abs() < 1e-12 && (t - t219).abs() < 1e-12, "vertical FOV preserved");
        assert!((r43 / t43 - 4. / 3.).abs() < 1e-6 && (r219 / t219 - 2560. / 1080.).abs() < 1e-6);
        assert!(r43 < r && r219 > r);
        // vertical FOV of 80 deg horizontal at 16:9 is ~50.5 deg
        assert!((2. * (t / NEAR as f64).atan().to_degrees() - 50.5).abs() < 0.2);
    }
    #[test] fn pitch_is_clamped_to_view_limits() { assert_eq!(clamp_pitch(120.), 89.9); assert_eq!(clamp_pitch(-120.), -89.9); assert_eq!(clamp_pitch(10.), 10.); }
    #[test] fn interp_to_never_overshoots_and_instant_sentinel_snaps() {
        assert_eq!(interp_to(0., 10., 1., 1.0e11), 10.);
        assert_eq!(interp_to(0., 10., 1., 30.), 10.);
        assert!((interp_to(0., 10., 0.1, 8.) - 8.).abs() < 1e-5);
        assert_eq!(interp_to(3., 3., 0.016, 8.), 3.);
    }

    // ---- crouch transition ----
    #[test] fn crouch_offset_interpolates_at_serialized_speed_then_settles() {
        let mut c = Camera::default();
        settle(&mut c, &MELEE, Vec3::ZERO, false, 0., 0., 1.);
        assert!((c.view_target_offset_cm().z - 70.).abs() < 1e-3);
        let mut prev = 70.;
        // one 1/120 s step: z moves 8/120 of the way from 70 to 30
        c.update(&MELEE, Input { feet: Vec3::ZERO, crouched: true, yaw_deg: 0., pitch_deg: 0., dt: 1. / 120. }, trace_ground);
        assert!((c.view_target_offset_cm().z - (70. + (30. - 70.) * 8. / 120.)).abs() < 1e-3);
        for _ in 0..240 { c.update(&MELEE, Input { feet: Vec3::ZERO, crouched: true, yaw_deg: 0., pitch_deg: 0., dt: 1. / 120. }, trace_ground); let z = c.view_target_offset_cm().z; assert!(z <= prev + 1e-6); prev = z; }
        assert!((prev - 30.).abs() < 0.5);
        // X/Y shoulder offsets unchanged by crouching
        assert!((c.view_target_offset_cm().x - 38.).abs() < 1e-3 && (c.view_target_offset_cm().y - 31.).abs() < 1e-3);
    }
    #[test] fn crouch_lowers_eye_smoothly_and_standing_returns() {
        let mut c = Camera::default();
        let stand = settle(&mut c, &MELEE, Vec3::ZERO, false, 0., 0., 1.).eye.y;
        let mut last = stand; let mut max_step = 0f32;
        for _ in 0..360 { let y = c.update(&MELEE, Input { feet: Vec3::ZERO, crouched: true, yaw_deg: 0., pitch_deg: 0., dt: 1. / 120. }, trace_ground).eye.y; max_step = max_step.max((last - y).abs()); last = y; }
        // settled: standing capsule centre 0.75 + crouch offset 0.30 = 1.05 (drop of exactly the serialized 40 cm)
        assert!((last - 1.05).abs() < 0.01, "{last}");
        assert!((stand - last - 0.40).abs() < 0.01, "crouch lowers camera by the serialized offset change");
        assert!(max_step < 0.4 * 8. / 120. + 1e-4, "no pops: {max_step}");
        for _ in 0..360 { last = c.update(&MELEE, Input { feet: Vec3::ZERO, crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 120. }, trace_ground).eye.y; }
        assert!((last - stand).abs() < 0.01);
    }
    #[test] fn crouch_transition_reaches_same_value_at_different_frame_rates() {
        let mut vals = vec![];
        for rate in [30., 60., 144.] {
            let mut c = Camera::default();
            for _ in 0..rate as usize { c.update(&MELEE, Input { feet: Vec3::ZERO, crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / rate }, trace_ground); }
            for _ in 0..(rate * 0.5) as usize { c.update(&MELEE, Input { feet: Vec3::ZERO, crouched: true, yaw_deg: 0., pitch_deg: 0., dt: 1. / rate }, trace_ground); }
            vals.push(c.view_target_offset_cm().z);
        }
        // FInterpTo-style stepping is only approximately rate independent; require agreement within 3 cm of 40.
        assert!(vals.iter().all(|v| (v - vals[2]).abs() < 3.), "{vals:?}");
    }

    // ---- collision ----
    #[test] fn camera_never_goes_below_ground_for_any_view_angle() {
        for yaw in [0., 33., 180., 271.] { for pitch in (-89..=89).step_by(4) { for crouched in [false, true] {
            let mut c = Camera::default();
            let v = settle(&mut c, &MELEE, Vec3::new(3., 0., -2.), crouched, yaw, pitch as f32, 1.5);
            assert!(v.eye.y >= 0.08 - 1e-3, "yaw {yaw} pitch {pitch} crouch {crouched}: eye.y {}", v.eye.y);
        }}}
    }
    #[test] fn looking_up_pulls_camera_in_and_hard_clip_sits_on_probe_sphere() {
        let mut c = Camera::default();
        let v = settle(&mut c, &MELEE, Vec3::ZERO, false, 0., 60., 2.);
        assert!(v.desired.y < 0., "boom would go under the ground");
        assert!(v.blocked_pct < 0.9 && v.blocked_pct > 0.4, "{}", v.blocked_pct);
        // main feeler: 8 cm sphere, push-out 0.2 cm -> resting height is ~8 cm (+tiny push-out)
        assert!((v.eye.y - 0.08).abs() < 0.01, "{}", v.eye.y);
    }
    #[test] fn penetration_blends_out_gradually_but_blocks_instantly() {
        let mut c = Camera::default();
        settle(&mut c, &MELEE, Vec3::ZERO, false, 0., 60., 1.);
        let blocked = c.update(&MELEE, Input { feet: Vec3::ZERO, crouched: false, yaw_deg: 0., pitch_deg: 60., dt: 1. / 120. }, trace_ground).blocked_pct;
        // Pitch jumps level: clear line, but the soft blocked fraction recovers over time (no pop).
        let first = c.update(&MELEE, Input { feet: Vec3::ZERO, crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 120. }, trace_ground);
        assert!(first.blocked_pct < 1. && first.blocked_pct >= blocked - 1e-3 );
        let last = settle(&mut c, &MELEE, Vec3::ZERO, false, 0., 0., 2.);
        assert!((last.blocked_pct - 1.).abs() < 1e-3);
        // and blocks in at once when the view swings down into the ground
        let v = c.update(&MELEE, Input { feet: Vec3::ZERO, crouched: false, yaw_deg: 0., pitch_deg: 80., dt: 1. / 120. }, trace_ground);
        assert!(v.eye.y >= 0.079);
    }
    #[test] fn wall_blocks_camera_and_does_not_penetrate() {
        // wall plane at z = 1.0 (camera boom heads toward +z at yaw 0): sphere centre must stay in front of it.
        let wall = |from: Vec3, to: Vec3, r: f32| -> Option<f32> {
            let limit = 1.0 - r;
            if from.z >= limit { return Some(0.); }
            if to.z <= limit { return None; }
            Some((limit - from.z) / (to.z - from.z))
        };
        let mut c = Camera::default();
        let mut v = View::default();
        for _ in 0..240 { v = c.update(&MELEE, Input { feet: Vec3::new(0., 3., 0.), crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 120. }, wall); }
        assert!(v.eye.z <= 1.0 - 0.08 + 1e-3, "{}", v.eye.z);
        assert!(v.blocked_pct < 0.5);
    }
    #[test] fn pawn_hides_only_when_boom_is_almost_fully_blocked() {
        let mut c = Camera::default();
        let v = settle(&mut c, &MELEE, Vec3::ZERO, false, 0., -12., 2.);
        assert!(!v.hide_pawn);
        let all_blocked = |from: Vec3, _to: Vec3, _r: f32| -> Option<f32> { let _ = from; Some(0.02) };
        let mut c = Camera::default();
        let v = c.update(&MELEE, Input { feet: Vec3::ZERO, crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 60. }, all_blocked);
        assert!(v.hide_pawn && v.blocked_pct <= 0.15);
    }

    // ---- penetration avoidance @1404f85a0 ----
    #[test] fn soft_feelers_blend_in_over_the_blend_in_time_and_the_main_feeler_snaps() {
        let mut c = Camera::default();
        settle(&mut c, &MELEE, Vec3::new(0., 3., 0.), false, 0., 0., 1.);
        // only the zero-extent side feelers (1..4) hit at half the ray: soft = 0.5 - push-out, hard stays 1
        let side_only = |_a: Vec3, _b: Vec3, r: f32| if r == 0. { Some(0.5) } else { None };
        let step = |c: &mut Camera, tr: &dyn Fn(Vec3, Vec3, f32) -> Option<f32>| c.update(&MELEE, Input { feet: Vec3::new(0., 3., 0.), crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 120. }, tr).blocked_pct;
        let mut seq = vec![];
        for _ in 0..120 { seq.push(step(&mut c, &side_only)); }
        let soft = *seq.last().unwrap();
        assert!(soft < 0.5 && soft > 0.45, "{soft}");
        // never a jump: each frame closes dt / 0.1 = 1/12 of the remaining gap
        let first = seq.iter().position(|p| *p < 0.99999).unwrap();
        assert!((1. - seq[first]) <= (1. - soft) / 12. + 1e-4, "blend-in step {} of gap {}", 1. - seq[first], 1. - soft);
        assert!(seq.windows(2).all(|w| w[1] <= w[0] + 1e-6));
        // the main feeler (8 cm sphere) blocking deeper snaps at once
        let all = |_a: Vec3, _b: Vec3, _r: f32| Some(0.2);
        let p = step(&mut c, &all);
        assert!(p < 0.2 && p > 0.15, "hard snap {p}");
    }
    #[test] fn world_weight_does_not_scale_the_hit_fraction() {
        static LIGHT: [Feeler; 1] = [Feeler { adjust_pitch: 0., adjust_yaw: 0., world_weight: 0.1, pawn_weight: 0.1, extent: 8., trace_interval: 0 }];
        static HEAVY: [Feeler; 1] = [Feeler { adjust_pitch: 0., adjust_yaw: 0., world_weight: 1., pawn_weight: 1., extent: 8., trace_interval: 0 }];
        let (a, b) = (Mode { feelers: &LIGHT, ..MELEE }, Mode { feelers: &HEAVY, ..MELEE });
        let tr = |_a: Vec3, _b: Vec3, _r: f32| Some(0.4);
        let (mut ca, mut cb) = (Camera::default(), Camera::default());
        let inp = Input { feet: Vec3::new(0., 3., 0.), crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 60. };
        let (va, vb) = (ca.update(&a, inp, tr), cb.update(&b, inp, tr));
        assert!((va.blocked_pct - vb.blocked_pct).abs() < 1e-7 && va.blocked_pct < 0.4, "{} {}", va.blocked_pct, vb.blocked_pct);
    }
    #[test] fn feeler_rays_rotate_like_rotate_angle_axis() {
        let (f, r, u) = view_basis(30., 10.);
        let d = feeler_dir(f, r, u, 16., 0.);
        assert!(d.dot(r) > 0.27 && (d.length() - 1.).abs() < 1e-5, "+yaw turns toward the right");
        let d = feeler_dir(f, r, u, 0., 20.);
        assert!(d.dot(u) < -0.34 && (d.length() - 1.).abs() < 1e-5, "+pitch about the ray's Y turns it down (UE RotateAngleAxis)");
    }
    #[test] fn safe_location_is_the_aim_line_point_inside_the_live_capsule() {
        // capsule radius 42 cm (AFortPawn ctor @1407c9ff0), standing half height 75 -> hemisphere centres at 0.42 / 1.08 m
        let (f, _, _) = view_basis(0., 0.);
        // aim line 31 cm right of the axis at 1.1 m: inside the capsule -> the line point itself
        let s = safe_location(&MELEE, Vec3::ZERO, false, Vec3::new(0.31, 1.1, 2.42), f);
        assert!((s - Vec3::new(0.31, 1.1, 0.)).length() < 1e-5, "{s:?}");
        // at 1.45 m the line point is outside the top hemisphere (centre 1.08): onto the surface, then 8.2 cm inward
        let s = safe_location(&MELEE, Vec3::ZERO, false, Vec3::new(0.31, 1.45, 2.42), f);
        assert!(((s - Vec3::new(0., 1.08, 0.)).length() - (0.42 - 0.082)).abs() < 1e-4, "{s:?}");
        // 1 m to the side: clamped onto the capsule and pushed 8.2 cm back in
        let s = safe_location(&MELEE, Vec3::ZERO, false, Vec3::new(1., 1., 2.42), f);
        assert!((s - Vec3::new(0.42 - 0.082, 1., 0.)).length() < 1e-4, "{s:?}");
        // crouched: the live capsule is half height 50 centred 50 cm above the feet (hemisphere centres 0.42 / 0.58): the 1.1 m point is outside it
        let s = safe_location(&MELEE, Vec3::ZERO, true, Vec3::new(0.31, 1.1, 2.42), f);
        assert!(((s - Vec3::new(0., 0.58, 0.)).length() - (0.42 - 0.082)).abs() < 1e-4 && s.y > 0.58, "{s:?}");
    }
    #[test] fn pawn_hide_is_strict() {
        // vfunc_69 @1405020b0: hidden only when DistBlockedPct < HidePawnPenetrationPercent; Base has 0, so even a fully blocked view keeps it
        let all = |_a: Vec3, _b: Vec3, _r: f32| Some(0.);
        let v = Camera::default().update(&BASE, Input { feet: Vec3::ZERO, crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 60. }, all);
        assert!(v.blocked_pct == 0. && !v.hide_pawn);
    }

    // ---- origin lag / snap ----
    #[test] fn forward_origin_lag_follows_serialized_speed_and_lateral_is_instant() {
        let mut c = Camera::default();
        settle(&mut c, &MELEE, Vec3::ZERO, false, 0., 0., 1.);
        // step the pawn 1 m forward (-z) and 1 m right (+x) in one frame at dt=1/60
        c.update(&MELEE, Input { feet: Vec3::new(1., 0., -1.), crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 60. }, trace_ground);
        let rem = c.origin - Vec3::new(1., 0.75, -1.);
        assert!(rem.x.abs() < 1e-5, "lateral axis has a 1e11 (instant) speed: {rem:?}");
        assert!((rem.z - 0.5).abs() < 1e-4, "forward lag keeps 1 - 30/60 of the gap: {rem:?}");
    }
    #[test] fn large_teleport_snaps_instead_of_gliding() {
        let mut c = Camera::default();
        settle(&mut c, &MELEE, Vec3::ZERO, false, 0., 0., 1.);
        let v = c.update(&MELEE, Input { feet: Vec3::new(100., 0., 100.), crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 60. }, trace_ground);
        assert!((v.eye - Vec3::new(100.31, 1.45, 100. + 2.42)).length() < 0.05, "{:?}", v.eye);
    }
    #[test] fn steady_walking_keeps_a_constant_follow_distance() {
        let mut c = Camera::default();
        let mut feet = Vec3::ZERO; let mut gaps = vec![];
        for i in 0..600 { feet.z -= 5.5 / 120.; let v = c.update(&MELEE, Input { feet, crouched: false, yaw_deg: 0., pitch_deg: -12., dt: 1. / 120. }, trace_ground); if i > 300 { gaps.push(v.eye.z - feet.z); } }
        let (mn, mx) = gaps.iter().fold((f32::MAX, f32::MIN), |a, g| (a.0.min(*g), a.1.max(*g)));
        assert!(mx - mn < 1e-3, "follow distance must not oscillate: {mn} {mx}");
    }
    // ---- fixed-step split: advance (tick) + compose (frame) ----
    #[test] fn compose_of_the_ticked_state_is_exactly_the_update_view() {
        let mut c = Camera::default();
        for (feet, yaw, pitch, crouch) in [(Vec3::ZERO, 0., 0., false), (Vec3::new(1., 0., -2.), 33., -20., false), (Vec3::new(1., 0., -2.), 33., 60., true), (Vec3::new(1., 3., -2.), 200., 80., false)] {
            for _ in 0..20 {
                let v = c.update(&MELEE, Input { feet, crouched: crouch, yaw_deg: yaw, pitch_deg: pitch, dt: 1. / 30. }, trace_ground);
                let w = Camera::compose(&MELEE, &c.state(), feet, yaw, pitch, trace_ground);
                assert!((v.eye - w.eye).length() < 1e-5, "{:?} vs {:?}", v.eye, w.eye);
                assert!((v.blocked_pct - w.blocked_pct).abs() < 1e-5 && v.hide_pawn == w.hide_pawn);
            }
        }
    }
    #[test] fn compose_never_puts_the_eye_under_the_ground_when_pitch_changes_between_ticks() {
        let mut c = Camera::default();
        for _ in 0..60 { c.advance(&MELEE, Input { feet: Vec3::ZERO, crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 30. }, trace_ground); }
        for pitch in (0..=89).step_by(3) { let v = Camera::compose(&MELEE, &c.state(), Vec3::ZERO, 0., pitch as f32, trace_ground); assert!(v.eye.y >= 0.08 - 1e-3, "pitch {pitch}: {}", v.eye.y); }
    }
    #[test] fn cam_state_lerp_endpoints() {
        let a = CamState { origin: Vec3::ZERO, vt: Vec3::ZERO, penetration: 0.2, crouched: false }; let b = CamState { origin: Vec3::ONE, vt: Vec3::splat(2.), penetration: 1., crouched: false };
        assert_eq!(CamState::lerp(&a, &b, 0.), a); assert_eq!(CamState::lerp(&a, &b, 1.), b);
        let m = CamState::lerp(&a, &b, 0.5); assert!((m.origin - Vec3::splat(0.5)).length() < 1e-6 && (m.penetration - 0.6).abs() < 1e-6);
    }
}




#[cfg(test)]
mod terrain_tests {
    use super::*;
    use glam::Vec2;
    /// TERRAIN: the boom sweep stops on the landscape (here a 30 deg slope rising along +x, 5 m high at x = 0) like on the plate, with the same fraction.
    #[test] fn the_ground_sweep_stops_on_the_landscape() {
        let k = 30f32.to_radians().tan();
        let hf = fn_terrain::Heightfield::synthetic(Vec2::splat(-3000.), 100., 2, 31, |x, _| 500. + k * x);
        let w = fn_terrain::World { hf, map: fn_terrain::Mapping { centre_cm: [0.; 3], anchor: Vec3::ZERO, ref_z_cm: 0. } };
        // straight down at x = 0: the sphere (r 0.1) touches the plane when its centre is r / cos 30 above the surface
        let f = trace_ground_on(Some(&w), Vec3::new(0., 10., 2.), Vec3::new(0., 0., 2.), 0.1).unwrap();
        let want = (10. - (5. + 0.1 / 30f32.to_radians().cos())) / 10.;
        assert!((f - want).abs() < 2e-3, "{f} vs {want}");
        // horizontally into the slope at y = 6: the surface reaches y = 6 at x = 1 / tan 30
        let f = trace_ground_on(Some(&w), Vec3::new(-5., 6., 2.), Vec3::new(5., 6., 2.), 0.).unwrap();
        assert!((f * 10. - 5. - 1. / k).abs() < 0.02, "{}", f * 10. - 5.);
        assert!(trace_ground_on(Some(&w), Vec3::new(-20., 9., 2.), Vec3::new(-10., 9., 2.), 0.1).is_none());
        assert_eq!(trace_ground_on(None, Vec3::new(0., 1., 0.), Vec3::new(0., -1., 0.), 0.), Some(0.5));
    }
}
#[cfg(test)]
mod follow_tests {
    use super::*;
    #[test] fn frame_follow_lags_like_the_real_client_at_any_tick_rate() {
        for (fps, lag_real) in [(60f32, 6.83f32), (144., 10.82)] {
            let mut c = Camera::default(); let mut f: Option<CamState> = None; let (mut feet, mut acc) = (Vec3::ZERO, 0f32); let v = 4.1; let dt = 1. / fps;
            for _ in 0..(3. * fps) as usize {
                feet.z -= v * dt; acc += dt;
                if acc >= 1. / 30. - 1e-6 { acc -= 1. / 30.; c.advance(&MELEE, Input { feet, crouched: false, yaw_deg: 0., pitch_deg: 0., dt: 1. / 30. }, trace_ground); }
                f = Some(CamState::follow(&MELEE, f, &c.state(), feet, 0., 0., dt));
            }
            let lag = (f.unwrap().origin.z - (feet.z)) * 100.;
            assert!((lag - lag_real).abs() < 0.05, "{fps} fps: rendered pivot lag {lag} cm vs real {lag_real}");
            assert!((c.state().origin.z - feet.z).abs() * 100. < 0.01, "the 30 Hz tick alone has no lag");
        }
    }
}
