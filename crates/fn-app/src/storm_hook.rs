//! STORM HOOK (application half): loads the storm assets once (`fn_matshader::storm`), maps the zone from the Unreal level coordinates onto the port's baseplate and appends
//! the wall + cloud draws to the frame's FX frame (`Renderer::set_fx_frame`). The state (centre, radius, fade) is `fn_game::storm_state::StormState` on the `GameSim`
//! (F2 `storm`), the numbers and uniform contents are `fn_render::storm`.
//!
//! Space mapping (the same as the props / backdrop, see `fn_render::backdrop::to_game`): a UE world point `p` (cm) lands at port `anchor + ((p.x - c.x), (p.z - ref), (p.y - c.y)) *
//! 0.01` with `c` = the layout centre of the loaded props and `ref` = the terrain height under it. The FX pass works in "Unreal axes, port centimetres" (`fn_fx::view::to_ue`), so
//! the whole storm is shifted by `offset = (anchor.x * 100 - c.x, anchor.z * 100 - c.y, -ref)`: the zone centre, the cloud actor and `MPC_SafeZoneClouds.SafeZoneLoc` all move by it.
//! Without the real props (no layout centre) the offset is 0: the storm keeps its Unreal coordinates and the spawn lies 410 m from the first phase's centre.
use fn_fx::view::ViewParams;
use fn_game::storm_state::StormState;
use fn_matshader::storm::StormWorld;
use fn_render::fx::{FxFrame, FxRenderer};
use fn_render::storm::StormParams;
use glam::Vec3;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapping { center_cm: [f32; 3], anchor: Vec3, ground_ref_cm: f32 }
static MAPPING: Mutex<Option<Mapping>> = Mutex::new(None);

/// Called by the loader with the props layout centre (UE cm) and the anchor it lands on; the terrain re-base is added by [`set_ground_ref`].
pub fn set_mapping(center_cm: [f32; 3], anchor: Vec3) { if let Ok(mut m) = MAPPING.lock() { *m = Some(Mapping { center_cm, anchor, ground_ref_cm: m.map_or(0., |x| x.ground_ref_cm) }); } }
/// Called by the loader once the backdrop knows the terrain height under the layout centre (`BackdropData::ground_ref_cm`).
pub fn set_ground_ref(ground_ref_cm: f32) { if let Ok(mut m) = MAPPING.lock() { if let Some(x) = m.as_mut() { x.ground_ref_cm = ground_ref_cm; } } }

/// UE level cm -> FX space cm, a pure translation (see the module docs).
pub fn offset_cm() -> [f32; 3] {
    match MAPPING.lock().ok().and_then(|m| *m) {
        Some(m) if m.center_cm != [0.; 3] => [m.anchor.x * 100. - m.center_cm[0], m.anchor.z * 100. - m.center_cm[1], -m.ground_ref_cm],
        _ => [0.; 3],
    }
}

#[derive(Default)]
pub struct StormHook { pub game: StormMatch, world: Option<StormWorld>, tried: bool, env_applied: bool,
    /// POSTFX: `CurrentOutsideSafeZonePPVBlend` (AFortPlayerPawnAthena +0x19F0) = the BlendWeight of `OutsideSafeZonePP0` (ctor 0).
    pp_blend: f32 }

impl StormHook {
    /// True when the assets should be loaded now: not tried yet and the storm is on.
    pub fn wants_load(&self, state: &StormState, in_match: bool) -> bool { !self.tried && (state.enabled || in_match) }
    pub fn load(&mut self, dir: Option<PathBuf>, fx: &mut FxRenderer, state: &StormState) {
        self.tried = true;
        let Some(dir) = dir else { eprintln!("[storm] no game install: no storm visuals"); return };
        match fn_pak::PakSet::open_install(&dir).map_err(|e| e.to_string()).and_then(|set| StormWorld::load(&set, fx)) {
            Ok(w) => {
                for n in &w.notes { eprintln!("[storm] {n}"); }
                let off = offset_cm();
                let (cx, cy) = (state.center_cm[0] + off[0], state.center_cm[1] + off[1]);
                eprintln!("[storm] zone radius {:.0} m, centre {:.0} m from the spawn (mapping offset {:.0}, {:.0}, {:.0} cm)", state.radius_m(), (cx * cx + (cy - 1200.0) * (cy - 1200.0)).sqrt() * 0.01, off[0], off[1], off[2]);
                self.world = Some(w);
            }
            Err(e) => eprintln!("[storm] the storm visuals are unavailable: {e}"),
        }
    }
    /// Advances the fade timeline and puts the storm draws in front of `out` (the farthest things first: the wall and the cloud sheet).
    /// `t` = the material times of the frame (world time, real time, world delta: the per-frame expressions use world time, UE4.16 ShaderBaseClasses.cpp:287).
    pub fn frame(&mut self, state: &mut StormState, feet_m: Vec3, view: &ViewParams, dt: f32, t: fn_fx::uniform::MaterialTime, out: &mut FxFrame) {
        if !self.env_applied {
            // dev overrides for scripted runs (which cannot type into the F2 bar): FN_STORM_RADIUS_M, FN_STORM_AT_SPAWN=1 (zone centred on the spawn), FN_STORM_OFF=1
            self.env_applied = true;
            if let Some(r) = fn_live::var("FN_STORM_RADIUS_M").ok().and_then(|s| s.parse::<f32>().ok()) { state.set_radius_m(r); }
            if fn_live::var_os("FN_STORM_AT_SPAWN").is_some() { let o = offset_cm(); state.center_cm = [0. - o[0], 1200. - o[1], state.center_cm[2]]; }
            if let Some(d) = fn_live::var("FN_STORM_SHIFT_Z_M").ok().and_then(|s| s.parse::<f32>().ok()) { state.center_cm[1] += d * 100.; } // port metres along +z (the spawn looks toward -z)
            if fn_live::var_os("FN_STORM_OFF").is_some() { state.turn_off(); }
        }
        if state.here_requested { state.here_requested = false; let o = offset_cm(); state.center_cm = [feet_m.x * 100. - o[0], feet_m.z * 100. - o[1], feet_m.y * 100. - o[2]]; } // F2 `storm here`: port metres -> UE level cm
        state.tick(dt);
        // POSTFX: the storm post-process of the pawn (1.8 exe): the 1 s server timer FUN_140404530 sets bIsOutsideSafeZone = 2D distance to the
        // SafeZoneIndicator > Radius, OnRep_IsOutsideSafeZone @140440020 sets TargetOutsideSafeZonePPVBlend = 1 / 0, AFortPlayerPawnAthena::Tick @14044ec10
        // eases Current = FInterpTo(Current, Target, DeltaTime, OutsideSafeZoneBlendSpeed 2.0) into OutsideSafeZonePP0->BlendWeight. Standalone: the target
        // follows the zone test every frame (the 1 s timer granularity is not modelled). No indicator (storm off) = inside.
        {
            let off = offset_cm();
            let (dx, dy) = (feet_m.x * 100. - off[0] - state.center_cm[0], feet_m.z * 100. - off[1] - state.center_cm[1]);
            let target = if state.enabled && (dx * dx + dy * dy).sqrt() > state.radius_cm { 1. } else { 0. };
            self.pp_blend = finterp_to(self.pp_blend, target, dt, 2.0);
        }
        let Some(w) = &mut self.world else { return };
        // storm off = no indicator / storm actor (this replaces the old MasterOpacity <= 0 skip inside StormWorld::frame, which also hid the wall during the fade-in)
        if !state.enabled { return; }
        let off = offset_cm();
        let c = state.center_cm;
        let params = StormParams { center_cm: [c[0] + off[0], c[1] + off[1], c[2] + off[2]], radius_cm: state.radius_cm, master_opacity: state.master_opacity() };
        let mut f = w.frame(view, &params, off, t);
        if fn_live::var_os("FN_STORM_DEBUG").is_some() { eprintln!("[storm] frame: {} draws, params {:?}, eye (FX cm) {:?}", f.draws.len(), params, fn_fx::view::to_ue(view.eye)); }
        // every draw carries its own translucency sort data (FxDraw::sort): the order here only matters for the opaque (masked) pass
        f.append(out);
        *out = f;
    }
}

impl StormHook {
    /// POSTFX: this frame's BlendWeight of the storm post-process component (fn_render::RenderOptions::storm_pp_weight).
    pub fn pp_weight(&self) -> f32 { if fn_live::var_os("FN_STORM_PP_FULL").is_some() { 1. } else { self.pp_blend } }
}
/// `FMath::FInterpTo` (UnrealMath.cpp): speed <= 0 -> target; |dist| < KINDA_SMALL_NUMBER^2 -> target; else current + dist * clamp(dt * speed, 0, 1).
fn finterp_to(current: f32, target: f32, dt: f32, speed: f32) -> f32 {
    if speed <= 0. { return target; }
    let dist = target - current;
    if dist * dist < 1.0e-8 { return target; }
    current + dist * (dt * speed).clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_offset_moves_the_layout_centre_onto_the_anchor() {
        set_mapping([1000., 2000., 500.], Vec3::new(0., 0., -10.));
        set_ground_ref(300.);
        let off = offset_cm();
        // UE point = the layout centre -> FX space = anchor * 100 with Y/Z swapped (x, z-port = ue y), height = ue z - ref
        let p = [1000. + off[0], 2000. + off[1], 500. + off[2]];
        assert_eq!(p, [0., -1000., 200.]);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BotZone { pub center: Vec3, pub radius: f32, pub next_center: Vec3, pub next_radius: f32, pub shrinking: bool, pub phase: usize, pub damage_fraction: f32 }

pub const STORM_DEATH_CAUSE: fn_ui::screens::kill_feed::DeathCause = fn_ui::screens::kill_feed::DeathCause::OutsideSafeZone;

#[derive(Default)]
pub struct StormMatch { zones: Option<fn_game::storm_zones::SafeZones>, clock: f32, last_drop_now: Option<f32>, scale: Option<f32>, in_match: bool, roster: u32, outside: bool, last_step: Option<u8>, wall_key: Option<u32>, death_place: Option<u32>, alert: Option<fn_ui::screens::hud_cooked::ActionAlert>, game_over_shown: bool }

const KEY_WALL_STATIONARY: u32 = 0x5354_0001;
const KEY_WALL_MOVING: u32 = 0x5354_0002;
const KEY_STORM_LOOP: u32 = 0x5354_0003;
fn storm_cue(name: &str) -> &'static str { fn_audio_sink::STORM_CUES.iter().copied().find(|c| c.ends_with(name)).unwrap_or("") }

fn match_seed() -> u32 {
    match fn_live::var("FN_STORM_SEED").ok().and_then(|v| v.trim().parse::<u32>().ok()) { Some(s) => s, None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u32) }
}

impl StormMatch {
    pub fn zones_view(&self) -> Option<(f32, usize, Vec<[f32; 3]>)> { let z = self.zones.as_ref().filter(|z| z.active)?; Some((self.clock, z.phase, z.locations.clone())) }
    pub fn active(&self) -> bool { self.in_match }
    pub fn players_left(&self) -> Option<u32> {
        if crate::mp_hook::in_match() { return crate::mp_hook::players_alive(); }
        if !self.in_match { return None; }
        Some((self.roster + crate::bots_hook::roster()).saturating_sub(crate::kill_feed_hook::eliminations()))
    }
    pub fn bot_zone(&self) -> Option<BotZone> {
        let z = self.zones.as_ref().filter(|z| z.active)?;
        let i = z.ind?;
        let off = offset_cm();
        let g = |c: [f32; 3]| glam::Vec3::new((c[0] + off[0]) * 0.01, 0., (c[1] + off[1]) * 0.01);
        Some(BotZone { center: g(i.center), radius: i.radius * 0.01, next_center: g(i.next_center), next_radius: i.next_radius * 0.01, shrinking: i.state == Some(fn_game::storm_zones::ZoneState::Shrinking), phase: z.phase, damage_fraction: z.damage_fraction() })
    }
    pub fn hud_phase(&self, sim: &fn_game::GameSim) -> Option<(u8, f32)> {
        let d = sim.drop.as_ref()?;
        if let Some(z) = self.zones.as_ref() { return Some(z.phase_step(self.clock)); }
        use fn_game::drop_in::GamePhaseStep as S;
        let (s, t) = d.game_phase_step();
        Some((match s { S::None => 0, S::Warmup => 2, S::GetReady => 3, S::BusLocked => 4, S::BusFlying => 5, S::StormForming => 6 }, t))
    }
    pub fn frame(&mut self, sim: &mut fn_game::GameSim, arms: Option<&mut crate::arms::Arms>) -> Vec<fn_audio_sink::SoundEvent> {
        let mut out = vec![];
        if crate::mp_hook::in_match() { return out; }
        let Some(d) = sim.drop.as_ref() else {
            if self.in_match {
                for k in [KEY_WALL_STATIONARY, KEY_WALL_MOVING, KEY_STORM_LOOP] { out.push(fn_audio_sink::SoundEvent::ContainerLoopStop { key: k, fade: 0. }); }
                *self = StormMatch::default();
            }
            return out
        };
        if !self.in_match {
            *self = StormMatch { in_match: true, roster: 1, scale: fn_live::var("FN_STORM_TIMESCALE").ok().and_then(|v| v.trim().parse::<f32>().ok()).filter(|s| *s > 0.), ..Default::default() };
            crate::kill_feed_hook::reset_eliminations();
            crate::kill_feed_hook::set_won(false);
            sim.storm.turn_off();
            eprintln!("[storm] match: no safe zone until 60 s after the bus leaves the drop zone (1.8 OnAircraftExitedDropZone -> StartSafeZonePhase)");
        }
        let now = d.now;
        let delta = self.last_drop_now.map_or(0., |l| (now - l).max(0.));
        self.last_drop_now = Some(now);
        let scale = self.scale.unwrap_or(1.);
        self.clock += delta * scale;
        if let Some((s, _)) = self.hud_phase(sim) {
            if self.last_step != Some(s) {
                self.last_step = Some(s);
                if fn_ui::screens::game_phase::alert_sound(s) { out.push(fn_audio_sink::SoundEvent::BuildingCue { cue: storm_cue("/Fort_GamePhaseChange_Message_Athena_Cue"), pos: None }); }
            }
        }
        if self.zones.is_none() {
            if d.phase != fn_game::drop_in::GamePhase::SafeZones { return out; }
            let remaining = (d.safe_zones_start_time - now).max(0.);
            let seed = match_seed();
            self.zones = Some(fn_game::storm_zones::SafeZones::new(self.clock + remaining - fn_game::storm_zones::START_DELAY_S, seed));
            eprintln!("[storm] the bus left the drop zone: the storm forms in {remaining:.0} s (seed {seed}, time scale {scale})");
        }
        let alive = arms.as_ref().map_or(true, |a| a.health > 0.);
        let off = offset_cm();
        let f = sim.feet();
        let pawn = (alive && !sim.on_bus()).then(|| [f.x * 100. - off[0], f.z * 100. - off[1]]);
        let z = self.zones.as_mut().unwrap();
        let events = z.tick(self.clock, pawn);
        let mut arms = arms;
        for e in events {
            use fn_game::storm_zones::ZoneEvent as E;
            match e {
                E::Spawned => {
                    sim.storm.turn_on();
                    let l = &z.locations;
                    eprintln!("[storm] the eye forms: radius {:.0} cm at ({:.0}, {:.0}); final circle at ({:.0}, {:.0})", z.ind.map_or(0., |i| i.radius), l[0][0], l[0][1], l[l.len() - 1][0], l[l.len() - 1][1]);
                }
                E::Advanced { phase } => { let i = z.ind.unwrap(); eprintln!("[storm] phase {phase}: holds {:.0} s then shrinks to {:.0} cm at ({:.0}, {:.0}) over {:.0} s, damage {:.3} x max health / s", i.start_shrink - self.clock, i.next_radius, i.next_center[0], i.next_center[1], i.finish_shrink - i.start_shrink, z.damage_fraction()); }
                E::StateChanged(s) => {
                    eprintln!("[storm] indicator state {s:?}");
                    let moving = s == fn_game::storm_zones::ZoneState::Shrinking;
                    out.push(fn_audio_sink::SoundEvent::BuildingCue { cue: storm_cue(if moving { "/SafeZone_BeginShrinking_Cue" } else { "/SafeZone_HoldingBegin_Cue" }), pos: None });
                    let key = if moving { KEY_WALL_MOVING } else { KEY_WALL_STATIONARY };
                    if self.wall_key != Some(key) {
                        if let Some(k) = self.wall_key { out.push(fn_audio_sink::SoundEvent::ContainerLoopStop { key: k, fade: 0. }); }
                        out.push(fn_audio_sink::SoundEvent::ContainerLoop { cue: storm_cue(if moving { "/Shield_PowerLoop_Moving_Cue" } else { "/Shield_PowerLoop_Ambient_Cue" }), key, pos: Some(f) });
                        self.wall_key = Some(key);
                    }
                }
                E::Damage { fraction } => if let Some(a) = arms.as_deref_mut() {
                    let h0 = a.health;
                    a.health = fn_game::storm_zones::health_after_storm_tick(a.health, crate::arms::MAX_HEALTH, fraction);
                    eprintln!("[storm] outside the safe zone: {:.2} damage, health {h0} -> {} (shield {} untouched)", fraction * crate::arms::MAX_HEALTH, a.health, a.shield);
                    crate::kill_feed_hook::local_damaged(STORM_DEATH_CAUSE, fn_ui::screens::kill_feed::Killer::Themself, a.health);
                    out.push(fn_audio_sink::SoundEvent::BuildingCue { cue: storm_cue("/Storm_Player_Damage_Zaps_Cue"), pos: None });
                },
            }
        }
        if let Some(i) = z.ind { sim.storm.center_cm = i.center; sim.storm.radius_cm = i.radius.max(1.); }
        let outside = z.active && pawn.is_some() && !z.inside_tag;
        if outside != self.outside {
            self.outside = outside;
            if outside { self.alert = Some(fn_ui::screens::hud_cooked::ActionAlert::Storm); eprintln!("[storm] the player left the safe zone: AthenaPlayerActionAlert (storm)"); }
            out.push(fn_audio_sink::SoundEvent::BuildingCue { cue: storm_cue("/Shield_Passthrough_Cue"), pos: Some(f) });
            out.push(if outside { fn_audio_sink::SoundEvent::ContainerLoop { cue: storm_cue("/SafeZone_Storm_Loop_Cue"), key: KEY_STORM_LOOP, pos: None } } else { fn_audio_sink::SoundEvent::ContainerLoopStop { key: KEY_STORM_LOOP, fade: 1. } });
        }
        if let (Some(k), Some(i)) = (self.wall_key, z.ind) {
            let c = glam::Vec3::new((i.center[0] + off[0]) * 0.01, f.y, (i.center[1] + off[1]) * 0.01);
            let d = glam::Vec3::new(f.x - c.x, 0., f.z - c.z);
            let p = if d.length_squared() > 1e-6 { c + d.normalize() * i.radius * 0.01 } else { c };
            out.push(fn_audio_sink::SoundEvent::LoopMove { key: k, pos: p });
        }
        out
    }
}

impl StormMatch {
    pub fn storm_map(&self) -> Option<fn_ui::screens::hud_extra::StormMap> {
        let i = self.zones.as_ref()?.ind?;
        let s = fn_ui::screens::hud_extra::MAP_LAYER_SIZE;
        let c = |p: [f32; 3], r: f32| { let (u, v) = fn_hud_adapter::minimap_uv_ue(p[0], p[1]); [u, v, r / fn_hud_adapter::MAP_WORLD_SCALE_CM / s] };
        Some(fn_ui::screens::hud_extra::StormMap { current: c(i.center, i.radius), next: c(i.next_center, i.next_radius) })
    }
    pub fn game_over(&mut self, alive: bool) -> Option<fn_ui::screens::hud_cooked::GameOver> {
        use fn_ui::screens::hud_cooked::{DeathKind, GameOver};
        use fn_ui::screens::kill_feed::{DeathCause, Killer};
        if crate::kill_feed_hook::won() || (crate::mp_hook::active() && (alive || crate::mp_hook::team_alive()) && self.zones.is_some() && crate::mp_hook::teams_alive() == Some(1)) { return Some(GameOver::Won); }
        let Some((cause, killer)) = crate::kill_feed_hook::local_death() else { self.death_place = None; return None };
        let left = self.players_left();
        let place = *self.death_place.get_or_insert_with(|| left.map_or(1, |n| n + 1));
        let kind = match (cause, killer) { (DeathCause::OutsideSafeZone, _) => DeathKind::Storm, (DeathCause::FallDamage, _) | (_, Killer::Themself) => DeathKind::SelfInflicted, (_, Killer::Player(c)) => DeathKind::Killer(c.name), _ => DeathKind::Killer(String::new()) };
        Some(GameOver::Died { player: crate::kill_feed_hook::local_name(), kind, place, team_size: 1 })
    }
    pub fn cursor_mode(&self) -> bool { self.game_over_shown }
    pub fn set_cursor_mode(&mut self, on: bool) { self.game_over_shown = on; }
    pub fn take_alert(&mut self) -> Option<fn_ui::screens::hud_cooked::ActionAlert> { self.alert.take() }
    pub fn hud(&self, sim: &fn_game::GameSim, st: &mut fn_ui::screens::hud::HudState) {
        st.players_left = self.players_left().or(st.players_left);
        if crate::mp_hook::in_match() { return; }
        st.game_phase = self.hud_phase(sim);
        st.storm_map = self.storm_map();
    }
}
