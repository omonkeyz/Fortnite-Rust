//! HEAL FX (fn-app): the green burst the 1.8 BR pawn shows when a healing item applies (Medkit, Bandage, PurpleStuff), drawn with the real Cascade system
//! `P_Consume_Item` (exported to `<assets>/particles-healfx` by `fn-particles-export`) on its ORIGINAL cooked material shaders (`pickups_dxbc::Dxbc`, as foot_fx).
//!
//! RECOVERED (cooked data + exe):
//! * the effect is NOT a GameplayCue: `GA_Athena_Medkit`'s bytecode only adds / removes `GameplayCue.Athena.Equipping` (the use ring), and
//!   `GE_Athena_Bandage_Heal` / `GE_Athena_Medkit_Heal` carry no GameplayCues. It is an `AnimNotify_PlayParticleEffect` of the use montage (the ability plays
//!   `MontageInfo` = `ApplyMedkit_Athena`, rate 1): `PSTemplate = /Game/Effects/Fort_Effects/Gameplay/P_Consume_Item`, `SocketName = b_MF_Head`, offsets 0,
//!   `Attached` unserialized (= true, ctor), at montage time 10.0666 (LinkValue 10.06667 + TriggerTimeOffset -0.0001), the same moment as the TriggerAbility
//!   notify (heal applied) and the `EatHealth_Restore_Athena_Cue` sound. Nothing plays during the 10 s channel; a cancelled use never reaches the notify.
//!   `WrapBandages_Athena` (3.5086) and `ApplyPurple_Athena` (1.9968) carry the same notify; `ApplyShield_Athena` has no particle notify.
//! * `UAnimNotify_PlayParticleEffect::Notify` (exe 0x141fe85d0, = UE4.16 AnimNotify_PlayParticleEffect.cpp): an immortal (looping) template is refused;
//!   `Attached` -> `UGameplayStatics::SpawnEmitterAttached(PSTemplate, MeshComp, SocketName, LocationOffset, RotationOffset, KeepRelativeOffset, bAutoDestroy)`.
//! * socket `b_MF_Head` (`Fortnite_M_Avg_Player_Skeleton` SkeletalMeshSocket; the BR body mesh does not override it) = bone `head`, zero offset / rotation.
//! * `P_Consume_Item`: two world-space (non-local) sprite emitters (`M_Spinning_Star` x5, `M_Alarm` x3), one burst at t 0, lifetime 0.3-0.6 s, green
//!   ColorOverLife (about (0.07, 3.43, 1.18)); LOD distances 0 / 800 / 2500.
//!
//! The component follows the head bone (attached); its particles, being world-space, stay where they spawned. ASSUMED (not visible): the component rotation is
//! the pawn yaw rather than the head bone's full orientation (both emitters spawn on an isotropic sphere with radial velocity, so it does not change the look).
//! * [`init`] (from `app.rs::start_world_load`), [`on_notifies`] (from `drive_arms`, the arms' montage particle notifies), [`on_head_pose`] (from `scene.rs`
//!   after the pawn pose), [`fx_frame`] (FX pass), [`prebuild`] (loading screen). Kill switch `FN_HEALFX=0`; `FN_HEALFX_DIR` overrides the catalog folder.
use crate::pickups_dxbc::Dxbc;
use fn_anim::sequence::ParticleNotify;
use fn_fx::view::ViewParams;
use fn_particles::assets::Catalog;
use fn_particles::{Env, Instance, Transform};
use fn_render::fx::{FxFrame, FxRenderer};
use glam::{Mat3, Vec3};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const P_CONSUME_ITEM: &str = "/Game/Effects/Fort_Effects/Gameplay/P_Consume_Item";

/// The player skeleton's sockets the notify may name: (socket, bone, relative location in UE cm). Only `b_MF_Head` is used by the 1.8 consumable montages.
const SOCKETS: &[(&str, &str, [f32; 3])] = &[("b_MF_Head", "head", [0., 0., 0.])];

/// The skeleton bone + offset of `socket` (None = a socket the port does not know; the notify is then not spawned and reported).
pub fn socket_bone(socket: &str) -> Option<(&'static str, Vec3)> { SOCKETS.iter().find(|s| s.0 == socket).map(|s| (s.1, Vec3::from(s.2))) }

/// The catalog path (`/Game/.../P_X`) of a notify's object path (`/Game/.../P_X.P_X`).
pub fn system_path(template: &str) -> &str { template.split('.').next().unwrap_or(template) }

/// One spawned `UParticleSystemComponent` (auto-destroyed when its emitters finish).
struct Spawned { inst: Instance, socket: &'static str, offset: Vec3, attached: bool }
struct State {
    catalog: Option<Arc<Catalog>>, install: Option<PathBuf>, dxbc: Option<Result<Dxbc, String>>, live: Vec<Spawned>, pending: Vec<ParticleNotify>,
    /// Last head pose: (bone origin, UE cm; pawn yaw, UE degrees) and the camera eye (UE cm, for the LOD pick).
    head: Option<(Vec3, f32)>, eye: Vec3, serial: u64, errors: std::collections::HashSet<String>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
static WARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Loads `<root>/particles-healfx` (or `FN_HEALFX_DIR`); `install` = the 1.8 install whose paks hold the cooked materials.
pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) {
    let dir = fn_live::var_os("FN_HEALFX_DIR").map(PathBuf::from).filter(|d| d.join("index.json").is_file()).or_else(|| roots.iter().map(|r| r.join("particles-healfx")).find(|d| d.join("index.json").is_file()));
    // `FN_HEALFX=0`: off (A/B comparisons)
    let off = fn_live::var("FN_HEALFX").map_or(false, |v| v == "0");
    let catalog = match dir.as_ref().filter(|_| !off).map(|d| Catalog::load(d)) {
        Some(Ok(c)) if c.system(P_CONSUME_ITEM).is_some() => { eprintln!("[healfx] P_Consume_Item loaded from {}", dir.as_ref().unwrap().display()); Some(Arc::new(c)) }
        Some(Ok(_)) => { eprintln!("[healfx] the particles-healfx catalog lacks P_Consume_Item: no heal FX"); None }
        Some(Err(e)) => { eprintln!("[healfx] {e}: no heal FX"); None }
        None if off => { eprintln!("[healfx] off (FN_HEALFX=0)"); None }
        None => { eprintln!("[healfx] no `particles-healfx` folder in the asset roots (fn-particles-export --out <assets>/particles-healfx /Game/Effects/Fort_Effects/Gameplay/P_Consume_Item): no heal FX"); None }
    };
    if install.is_none() { eprintln!("[healfx] no install (paks): the heal FX materials cannot be built, nothing drawn"); }
    WARMED.store(false, std::sync::atomic::Ordering::Relaxed);
    *STATE.lock().unwrap() = Some(State { catalog, install, dxbc: None, live: vec![], pending: vec![], head: None, eye: Vec3::ZERO, serial: 0, errors: Default::default() });
}

fn yaw_rot(yaw_deg: f32) -> Mat3 { Mat3::from_rotation_z(yaw_deg.to_radians()) }

fn dxbc<'a>(slot: &'a mut Option<Result<Dxbc, String>>, install: &Option<PathBuf>, fx: &mut FxRenderer) -> Option<&'a mut Dxbc> {
    if slot.is_none() {
        let r = match install { Some(d) => Dxbc::load(d, fx), None => Err("no install".into()) };
        if let Err(e) = &r { eprintln!("[healfx] heal FX materials unavailable: {e}"); }
        *slot = Some(r);
    }
    slot.as_mut().and_then(|r| r.as_mut().ok())
}

fn draw(dx: &mut Dxbc, fx: &mut FxRenderer, cat: &Catalog, inst: &Instance, eye: [f32; 3], dt: f32, errors: &mut std::collections::HashSet<String>) {
    let mut d = vec![];
    inst.draw(eye, &mut d);
    let bounds = crate::particles::instance_bounds(inst, cat);
    for e in &d {
        if let Err(err) = dx.draw_emitter(fx, e, &inst.xf, &bounds, dt) {
            let k = format!("{}/{}", e.system, e.emitter);
            if errors.insert(k.clone()) { eprintln!("[healfx] {k}: NOT DRAWN: {err}"); }
        }
    }
}

/// The arms' montage particle notifies of this frame (`AnimNotify_PlayParticleEffect`); spawned at the next head pose.
pub fn on_notifies(n: Vec<ParticleNotify>) {
    if n.is_empty() { return; }
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    if st.catalog.is_none() { return; }
    st.pending.extend(n);
}

/// The pawn's displayed head bone this frame: `head_ue` = bone origin (UE cm), `yaw_deg` = pawn yaw (UE degrees), `eye_ue` = camera. Spawns the pending
/// notifies there (`SpawnEmitterAttached`) and moves the attached components with the bone.
pub fn on_head_pose(head_ue: Vec3, yaw_deg: f32, eye_ue: Vec3) {
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    let Some(cat) = st.catalog.clone() else { return };
    st.head = Some((head_ue, yaw_deg)); st.eye = eye_ue;
    let rot = yaw_rot(yaw_deg);
    for s in st.live.iter_mut().filter(|s| s.attached && s.socket == "b_MF_Head") { s.inst.xf.pos = head_ue + rot * s.offset; s.inst.xf.rot = rot; }
    for n in std::mem::take(&mut st.pending) {
        let path = system_path(&n.template);
        let Some(sys) = cat.system(path) else { if st.errors.insert(path.to_string()) { eprintln!("[healfx] {path} is not in the particles-healfx catalog: not spawned"); } continue };
        let Some((bone, sock_off)) = socket_bone(&n.socket) else { if st.errors.insert(n.socket.clone()) { eprintln!("[healfx] unknown socket {}: {path} not spawned", n.socket); } continue };
        let _ = bone; // the only known socket rides the `head` bone (scene.rs feeds that bone)
        let off = sock_off + Vec3::from(n.location_offset) + fn_live::f32s("FN_HEALFX_OFFSET").filter(|v| v.len() == 3).map_or(Vec3::ZERO, |v| Vec3::new(v[0], v[1], v[2]));
        let pos = head_ue + rot * off;
        st.serial += 1;
        let inst = Instance::new(sys, Transform { pos, rot, scale: fn_live::f32("FN_HEALFX_SCALE", 1.) }, 0x4845_414c ^ st.serial, (eye_ue - pos).length());
        eprintln!("[healfx] {} spawned on {} at UE ({:.0}, {:.0}, {:.0})", path.rsplit('/').next().unwrap_or(path), n.socket, pos.x, pos.y, pos.z);
        st.live.push(Spawned { inst, socket: SOCKETS[0].0, offset: off, attached: n.attached });
    }
}

/// FIRST USE: opens the paks and builds the materials of `P_Consume_Item` under the loading screen (one throw-away frame).
pub fn prebuild(fx: &mut FxRenderer) {
    if WARMED.load(std::sync::atomic::Ordering::Relaxed) { return; }
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    let Some(cat) = st.catalog.clone() else { return };
    let t = std::time::Instant::now();
    let Some(dx) = dxbc(&mut st.dxbc, &st.install, fx) else { return };
    let view = ViewParams { eye: Vec3::new(0., 2., 3.), forward: Vec3::new(0., -0.5, -0.85).normalize(), up: Vec3::Y, tan_half: (1., 0.6), near_m: 0.1, far_m: 1000., width: 64, height: 64, time: 0., real_time: 0., delta_world: 1. / 60. };
    dx.begin(&view);
    if let Some(s) = cat.system(P_CONSUME_ITEM) {
        let mut i = Instance::new(s, Transform::identity(), 1, 100.);
        i.tick(1. / 30., &Env::none());
        draw(dx, fx, &cat, &i, [0., 2., 3.], 1. / 60., &mut st.errors);
    }
    let _ = dx.take_frame();
    WARMED.store(true, std::sync::atomic::Ordering::Relaxed);
    eprintln!("[healfx] warm-up: P_Consume_Item materials built in {:.0} ms", t.elapsed().as_secs_f32() * 1000.);
}

/// The live heal bursts of this frame, ticked by the world delta `dt`, as FX-pass draws appended to `out`.
pub fn fx_frame(fx: &mut FxRenderer, view: &ViewParams, dt: f32, out: &mut FxFrame) {
    let Ok(mut guard) = STATE.lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    let Some(cat) = st.catalog.clone() else { return };
    if st.live.is_empty() { return; }
    let env = Env::none();
    let tdt = dt * fn_live::f32("FN_HEALFX_TIME_SCALE", 1.);
    for s in st.live.iter_mut() { s.inst.tick(tdt, &env); }
    st.live.retain(|s| !s.inst.finished());
    if st.live.is_empty() { return; }
    let eye = [view.eye.x, view.eye.y, view.eye.z];
    let debug = dt > 0. && fn_live::var_os("FN_HEALFX_DEBUG").is_some();
    let State { dxbc: slot, install, live, errors, .. } = st;
    let Some(dx) = dxbc(slot, install, fx) else { return };
    dx.begin(view);
    for s in live.iter() {
        if debug { eprintln!("[healfx debug] {} live {} at UE ({:.0}, {:.0}, {:.0})", s.inst.system.name, s.inst.live_count(), s.inst.xf.pos.x, s.inst.xf.pos.y, s.inst.xf.pos.z); }
        draw(dx, fx, &cat, &s.inst, eye, dt, errors);
    }
    let fr = dx.take_frame();
    out.buffer_writes.extend(fr.buffer_writes);
    out.draws.extend(fr.draws);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_consume_item_notify_resolves_to_the_catalog_system_and_the_head_bone() {
        assert_eq!(system_path("/Game/Effects/Fort_Effects/Gameplay/P_Consume_Item.P_Consume_Item"), P_CONSUME_ITEM);
        assert_eq!(socket_bone("b_MF_Head"), Some(("head", Vec3::ZERO)));
        assert_eq!(socket_bone("RightHand"), None, "only the sockets the consumable montages use are mapped");
    }
}
