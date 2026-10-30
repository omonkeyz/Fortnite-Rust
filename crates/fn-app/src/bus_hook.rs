use crate::building_hook::{sim_to_world, world_to_fx};
use crate::pickups_dxbc::Dxbc;
use fn_audio_sink::{SoundEvent, DROP_CUES};
use fn_fx::view::ViewParams;
use fn_game::drop_in::{AerialPhase, DropEvent};
use fn_game::GameSim;
use fn_render::fx::{FxFrame, FxRenderer};
use fn_ui::screens::aerial_feedback::{AerialAction, AerialPrompt};
use glam::{Mat3, Mat4, Vec3};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

pub const BUS: &str = "/Game/Athena/Aircraft/Meshes/Vehicle_Athena_Bus.Vehicle_Athena_Bus";
pub const BALLOON: &str = "/Game/Athena/Aircraft/Meshes/Vehicle_Athena_Bus_Balloon_Full.Vehicle_Athena_Bus_Balloon_Full";
pub const APPARATUS: &str = "/Game/Athena/Aircraft/Meshes/Vehicle_Athena_Bus_Apparatus.Vehicle_Athena_Bus_Apparatus";
pub const FLAME: &str = "/Game/Missions/Primary/LaunchTheBalloon/Meshes_NoLOD/BalloonFlame.BalloonFlame";
pub const GENERATOR: &str = "/Game/Missions/Primary/LaunchTheBalloon/Meshes_NoLOD/LaunchTheBalloon_van_generator.LaunchTheBalloon_van_generator";
pub const BUS_YAW: f32 = -90.;
pub const FLAME_LOCATION: [f32; 3] = [0., -32., 520.];
pub const FLAME_SCALE: f32 = 1.6;
pub const GENERATOR_LOCATION: [f32; 3] = [76.5, 506., 360.];
pub const GENERATOR_YAW: f32 = -140.70;

const KEY_BUS_LOOP: u32 = 0x4255_0001;
const KEY_BUS_MUSIC: u32 = 0x4255_0002;
const KEY_SKYDIVE: u32 = 0x4255_0003;
const KEY_GLIDER: u32 = 0x4255_0004;
const SKYDIVE_FADE_IN: f32 = 1.0;
const SKYDIVE_FADE_OUT: f32 = 0.5;
const GLIDER_FADE_IN: f32 = 5.0;
const GLIDER_FADE_OUT: f32 = 1.0;
const GLIDER_GROUND_TRACE_CM: f32 = 10_000.;
const GLIDER_FULL_VOLUME_HEIGHT_CM: f32 = 6_000.;
const GLIDER_VOLUME_INTERP_SPEED: f32 = 1.;

#[derive(Default)]
struct State { install: Option<PathBuf>, dxbc: Option<Result<Dxbc, String>>, errors: HashSet<String>, bus_audio: bool, skydive_audio: bool, glider_audio: bool, glider_t: f32, glider_volume: f32 }
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); f(g.get_or_insert_with(State::default)) }

pub fn init(install: Option<PathBuf>) { with(|s| s.install = install); }

fn crt_rand(seed: &mut u32) -> f32 { *seed = seed.wrapping_mul(214_013).wrapping_add(2_531_011); ((*seed >> 16) & 0x7fff) as f32 / 32767. }

pub fn enabled(deterministic: bool) -> bool {
    match fn_live::var("FN_BUS").ok().as_deref().map(str::trim) { Some("1") => true, Some("0") => false, _ => !deterministic }
}

pub fn start_bus_now(sim: &mut GameSim) {
    let mut seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u32);
    let (a, b) = (crt_rand(&mut seed), crt_rand(&mut seed));
    sim.fly = fn_game::FlyMode::Walk;
    sim.controller = sim.new_controller();
    sim.start_match_on_bus(a, b);
    eprintln!("[bus] startbus: a fresh Battle Bus takes off");
}

pub fn begin_match(sim: &mut GameSim, deterministic: bool) {
    if !enabled(deterministic) { return; }
    let mut seed = match fn_live::var("FN_BUS_SEED").ok().and_then(|v| v.trim().parse::<u32>().ok()) { Some(v) => v, None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u32) };
    let (a, b) = (crt_rand(&mut seed), crt_rand(&mut seed));
    let skip_warmup = fn_live::var("FN_BUS_SKIP_WARMUP").map_or(false, |v| v.trim() == "1");
    if skip_warmup { sim.start_match_on_bus(a, b); } else {
        let s = crt_rand(&mut seed);
        sim.start_match_on_spawn_island(s, a, b, fn_game::drop_in::WARMUP_COUNTDOWN_DURATION);
        eprintln!("[bus] warmup on the spawn island: {:.0} s countdown at player start {:.0} of {}", fn_game::drop_in::WARMUP_COUNTDOWN_DURATION, s * fn_game::drop_in::WARMUP_STARTS.len() as f32, fn_game::drop_in::WARMUP_STARTS.len());
    }
    if let Some(d) = sim.drop.as_ref() { if let Some(ac) = d.aircraft.as_ref() {
        let p = &ac.path;
        eprintln!("[bus] match starts on the Battle Bus: yaw {:.1} deg, start ({:.0}, {:.0}, {:.0}) cm, flight {:.1} s, doors open at {:.1} s, last stop at {:.1} s", p.yaw_deg, p.start.x, p.start.y, p.start.z, p.duration, p.drop_start_offset, p.drop_end_offset);
    } }
}

fn cue(path: &str) -> &'static str { DROP_CUES.iter().copied().find(|c| *c == path).unwrap_or("") }

fn finterp_to(current: f32, target: f32, dt: f32, speed: f32) -> f32 {
    if speed <= 0. { return target; }
    let dist = target - current;
    if dist * dist < 1e-8 { return target; }
    current + dist * (dt * speed).clamp(0., 1.)
}

fn wind_loops(sim: &GameSim, dt: f32, s: &mut State, out: &mut Vec<SoundEvent>) {
    let c = sim.controller.component();
    let sky = !sim.on_bus() && c.in_aerial_mode() && !c.is_parachute_open();
    let glide = !sim.on_bus() && c.is_parachute_open();
    if sky != s.skydive_audio {
        s.skydive_audio = sky;
        out.push(if sky { SoundEvent::ContainerLoopFadeIn { cue: cue(DROP_CUES[3]), key: KEY_SKYDIVE, pos: None, fade: SKYDIVE_FADE_IN } } else { SoundEvent::ContainerLoopStop { key: KEY_SKYDIVE, fade: SKYDIVE_FADE_OUT } });
    }
    if glide != s.glider_audio {
        s.glider_audio = glide;
        s.glider_t = 0.;
        s.glider_volume = 1.;
        out.push(if glide { SoundEvent::ContainerLoopFadeIn { cue: cue(DROP_CUES[4]), key: KEY_GLIDER, pos: None, fade: GLIDER_FADE_IN } } else { SoundEvent::ContainerLoopStop { key: KEY_GLIDER, fade: GLIDER_FADE_OUT } });
    } else if glide {
        s.glider_t += dt;
        if c.is_parachute_forced_open() {
            let center = sim.feet() + Vec3::Y * (fn_game::drop_in::PAWN_HALF_HEIGHT / 100.);
            let floor = sim.floor_below(center);
            let height_cm = ((center.y - floor) * 100.).max(0.);
            let ground = if height_cm <= GLIDER_GROUND_TRACE_CM { height_cm } else { GLIDER_FULL_VOLUME_HEIGHT_CM };
            let target = (ground / GLIDER_FULL_VOLUME_HEIGHT_CM).clamp(0., 1.);
            s.glider_volume = finterp_to(s.glider_volume, target, dt, GLIDER_VOLUME_INTERP_SPEED);
            let fade = (s.glider_t / GLIDER_FADE_IN).clamp(0., 1.);
            out.push(SoundEvent::LoopGain { key: KEY_GLIDER, gain: fade * s.glider_volume });
        }
    }
}

pub fn frame(sim: &mut GameSim, dt: f32) -> Vec<SoundEvent> {
    let mut out = vec![];
    crate::statics_render::set_stream_hint(landing_hint(sim));
    crate::statics_render::set_read_hint(bus_ground(sim));
    with(|s| wind_loops(sim, dt, s, &mut out));
    let Some(d) = sim.drop.as_mut() else { out.retain(|e| !matches!(e, SoundEvent::ContainerLoopFadeIn { cue: "", .. })); return out };
    let events = d.take_events();
    let bus = d.aircraft_view().map(|(p, _)| p);
    let pawn = sim.feet();
    with(|s| {
        for e in &events {
            match e {
                DropEvent::ExitedAircraft { auto } => { eprintln!("[bus] left the bus ({})", if *auto { "auto-kick at the last stop" } else { "jump" }); out.push(SoundEvent::BuildingCue { cue: cue(DROP_CUES[2]), pos: None }); }
                DropEvent::DoorsOpened => eprintln!("[bus] doors open"),
                DropEvent::AircraftExitedDropZone => eprintln!("[bus] last stop: the bus left the drop zone"),
                DropEvent::FlightEnded => eprintln!("[bus] flight ended"),
                DropEvent::ParachuteOpened => { eprintln!("[bus] glider open"); out.push(SoundEvent::BuildingCue { cue: cue(DROP_CUES[5]), pos: Some(pawn) }); }
                DropEvent::ParachuteClosed => { eprintln!("[bus] glider closed"); out.push(SoundEvent::BuildingCue { cue: cue(DROP_CUES[6]), pos: Some(pawn) }); }
                DropEvent::Landed => eprintln!("[bus] landed at ({:.1}, {:.1}, {:.1})", pawn.x, pawn.y, pawn.z),
                DropEvent::EnteredAircraft => eprintln!("[bus] entered the Battle Bus"),
            }
        }
        match bus {
            Some(p) if !s.bus_audio => { s.bus_audio = true; out.push(SoundEvent::ContainerLoop { cue: cue(DROP_CUES[0]), key: KEY_BUS_LOOP, pos: Some(p) }); out.push(SoundEvent::ContainerLoop { cue: cue(DROP_CUES[1]), key: KEY_BUS_MUSIC, pos: Some(p) }); }
            Some(p) => { out.push(SoundEvent::LoopMove { key: KEY_BUS_LOOP, pos: p }); out.push(SoundEvent::LoopMove { key: KEY_BUS_MUSIC, pos: p }); }
            None if s.bus_audio => { s.bus_audio = false; out.push(SoundEvent::ContainerLoopStop { key: KEY_BUS_LOOP, fade: 0. }); out.push(SoundEvent::ContainerLoopStop { key: KEY_BUS_MUSIC, fade: 0. }); }
            None => {}
        }
    });
    out.retain(|e| !matches!(e, SoundEvent::BuildingCue { cue: "", .. } | SoundEvent::ContainerLoop { cue: "", .. } | SoundEvent::ContainerLoopFadeIn { cue: "", .. }));
    out
}

pub const HINT_MAX_DRIFT_M: f32 = 1000.;

fn landing_hint(sim: &GameSim) -> Option<Vec3> {
    if sim.drop.is_none() || sim.on_bus() { return None; }
    let c = sim.controller.component();
    if !c.is_skydiving() && !c.in_aerial_mode() { return None; }
    let p = sim.controller.render_position();
    let z = p.y + fn_game::Z_OFFSET;
    let g = sim.ground_y(p.x, z);
    let fall = (-sim.controller.vertical_speed).max(1.);
    let t = (sim.controller.height - g).max(0.) / fall;
    let d = (sim.controller.velocity * t).clamp_length_max(HINT_MAX_DRIFT_M);
    Some(Vec3::new(p.x + d.x, g, z + d.y))
}

fn bus_ground(sim: &GameSim) -> Option<Vec3> {
    if !sim.on_bus() { return None; }
    let (p, _) = sim.drop.as_ref()?.aircraft_view()?;
    Some(Vec3::new(p.x, sim.ground_y(p.x, p.z), p.z))
}

pub fn hud(sim: &GameSim, st: &mut fn_ui::screens::hud::HudState, jump_key: &str) {
    if sim.drop.is_none() { return; }
    crate::statics_render::set_on_bus(matches!(sim.aerial_phase(), AerialPhase::BusCantExit | AerialPhase::BusCanExit));
    let action = match sim.aerial_phase() { AerialPhase::BusCanExit => Some(AerialAction::Jump), AerialPhase::Skydiving => Some(AerialAction::DeployGlider), AerialPhase::Parachuting => Some(AerialAction::Skydive), _ => None };
    st.world.aerial = action.map(|action| AerialPrompt { key: jump_key.to_owned(), action });
    st.bus_map = sim.drop.as_ref().and_then(|d| Some((d.aircraft_view()?.0, d.aircraft.as_ref()?.path.yaw_deg))).map(|(p, yaw)| { let o = crate::storm_hook::offset_cm(); fn_ui::screens::hud_extra::MapIcon { uv: fn_hud_adapter::minimap_uv_ue(p.x * 100. - o[0], p.z * 100. - o[1]), yaw_deg: yaw } });
    if sim.on_bus() {
        st.visible.reticle = false; st.visible.player_health = false; st.visible.pickup_info = false; st.visible.quick_bar = false;
        st.kill_feed.clear(); st.world.ammo = None; st.world.equip = None; st.world.interaction = None; st.world.target = None;
    }
}

fn ue_yaw(deg: f32) -> Mat3 { Mat3::from_rotation_z(deg.to_radians()) }

pub fn prebuild(fx: &mut FxRenderer) -> bool {
    static AT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    with(|s| {
        if s.dxbc.is_none() { s.dxbc = Some(match s.install.as_ref() { Some(i) => Dxbc::load(i, fx), None => Err("no install".into()) }); return false; }
        let at = AT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(Ok(dx)) = s.dxbc.as_mut() {
            if let Some(mesh) = [BUS, BALLOON, APPARATUS, FLAME, GENERATOR].get(at) { let _ = dx.static_ready(fx, mesh); return false; }
            if at > 5 { return true; }
            let view = ViewParams { eye: Vec3::ZERO, forward: Vec3::NEG_Z, up: Vec3::Y, tan_half: (1.0, 0.5625), near_m: 0.1, far_m: 1000.0, width: 64, height: 64, time: 0.0, real_time: 0.0, delta_world: 0.0 };
            dx.begin(&view);
            for mesh in [BUS, BALLOON, APPARATUS, FLAME, GENERATOR] { let _ = dx.draw_static_item(fx, mesh, Vec3::new(0.0, 0.0, -1.0e7), Mat3::IDENTITY, [0.0; 3]); }
            let _ = dx.take_frame();
        }
        true
    })
}

pub fn fx_frame(fx: &mut FxRenderer, view: &ViewParams, sim: &GameSim, out: &mut FxFrame) {
    let Some(d) = sim.drop.as_ref() else { return };
    let Some(ac) = d.aircraft.as_ref() else { return };
    let Some((pos, _)) = d.aircraft_view() else { return };
    let root = Mat4::from_translation(world_to_fx(sim_to_world(pos))) * Mat4::from_mat3(ue_yaw(ac.path.yaw_deg));
    let bus = root * Mat4::from_mat3(ue_yaw(BUS_YAW));
    let parts = [
        (BUS, bus), (BALLOON, bus), (APPARATUS, bus),
        (FLAME, bus * Mat4::from_translation(Vec3::from(FLAME_LOCATION)) * Mat4::from_scale(Vec3::splat(FLAME_SCALE))),
        (GENERATOR, bus * Mat4::from_translation(Vec3::from(GENERATOR_LOCATION)) * Mat4::from_mat3(ue_yaw(GENERATOR_YAW))),
    ];
    with(|s| {
        if s.dxbc.is_none() { s.dxbc = Some(match s.install.as_ref() { Some(i) => Dxbc::load(i, fx), None => Err("no install".into()) }); }
        let State { dxbc, errors, .. } = s;
        let dx = match dxbc.as_mut().unwrap() { Ok(d) => d, Err(e) => { if errors.insert("dxbc".into()) { eprintln!("[bus] bus meshes unavailable: {e}"); } return; } };
        dx.begin(view);
        for (mesh, m) in parts {
            if let Err(e) = dx.draw_static_item(fx, mesh, m.w_axis.truncate(), Mat3::from_mat4(m), [0.; 3]) { if errors.insert(mesh.to_owned()) { eprintln!("[bus] {mesh}: NOT DRAWN: {e}"); } }
        }
        let fr = dx.take_frame();
        out.buffer_writes.extend(fr.buffer_writes);
        out.draws.extend(fr.draws);
        out.shadow_draws.extend(fr.shadow_draws);
        out.distance_field_objects.extend(fr.distance_field_objects);
    });
}
