//! Game logic facade of the Fortnite 1.8 runtime. Everything here is pure Rust with no OS, GPU or audio dependency: the front-end
//! (`fn-app`: winit + wgpu + audio + gamepad) feeds it a [`FrameInput`] per frame and reads the resulting pawn, camera and pose tree.
//!
//! * [`sim`]      [`GameSim`]: the old `App::update` (input -> latches -> movement -> animation graph -> camera, swing/harvest) as one `step`
//! * [`inputdev`] mouse-wheel pulses and XInput pad mapping (dead zones, sensitivity, Start latch); polling is a `PadBackend` of the app
//! * [`sim_tick`] the fixed `SIM_HZ` (30 Hz) tick: accumulator, latched taps, snapshots / interpolation, the headless `FixedSim` twin (old src/sim_tick.rs)
//! * [`tick_driver`] [`TickDriver`] / [`SimClock`]: what `fn-app` calls once per rendered frame (ticks + interpolation around a `GameSim`)
//! * [`host`]     window focus / cursor-capture / key-gating policy and raw-mouse look (old main.rs helpers)
//! * [`jump_air`] the headless twin of the controller + AdditiveJumps wiring used by the air-press regression tests
//! * [`jump_feel`] `--jump-feel-test`: the input -> screen chain of a jump at 30..240 Hz and the gravity / JumpZ experiment table
//! * [`time_audit`] `--time-test` bookkeeping and [`time_audit::MAX_DELTA_TIME`], the one global frame-time clamp (0.1 s)
//! * re-exports of the logic crates: [`movement`], [`camera`], [`props`], [`combat`], [`animgraph`], [`settings`]
//!
//! Platform seams (documented in `fn_settings::platform`): `PlatformHost` (display modes, vsync, vblank wait, cursor capture, focus),
//! `VideoHooks`/`InputHooks`/`AudioHooks` (renderer / input / audio sinks of the settings apply layer) and [`InputSource`] (per-frame input).
pub mod ads_view; // ADS HOOK (fn-game): hold / aim-down-sights camera blend + targeting speed limit
pub mod audio_events; // AUDIO HOOK (fn-game): renderer-free sound-moment detection (see fn-app README "Audio hooks")
pub mod foot_fx; // FOOT FX (fn-game): PlayerPawn_Athena_Generic walk / run / land dust gates (foot_fx.rs)
pub mod fall_damage; // FALL DAMAGE (fn-game): AthenaFallingDamage curve, FallingStartedZ apex, shield-first split (fall_damage.rs)
pub mod cosmetic_swap; // COSMETIC SWAP (fn-game): runtime BR skin / pickaxe / back bling / glider selection
pub mod gamepad_look; // GAMEPAD LOOK: right-stick look of FUN_140615dd0 with the cooked AthenaGamepadSettings
pub mod host;
pub mod inputdev;
pub mod pad_layout;
pub mod jump_air;
pub mod jump_feel;
pub mod pickup_items; // PICKUPS (fn-game): the BR item definitions a pickup carries (meshes, sounds, rarity)
pub mod drop_in;
pub mod containers; // CONTAINERS (fn-game): BR chests / ammo boxes: search, spawn selection, Athena loot roll (containers.rs)
pub mod pickups; // PICKUPS (fn-game): AFortPickup world item logic (toss physics, touch / Use pickup, fly to the pawn)
pub mod traps;
pub mod bounce;
pub mod sim;
pub mod storm_state; // STORM HOOK (fn-game): static safe zone (centre, radius, fade-in) the storm visuals read; F2 `storm`
pub mod storm_zones;
pub mod supply_drop;
pub mod sim_tick;
pub mod tick_driver;
pub mod time_audit;

pub use fn_animgraph as animgraph;
pub use fn_camera as camera;
pub use fn_combat as combat;
pub use fn_movement as movement;
pub use fn_props as props;
pub use fn_terrain as terrain;
pub use fn_settings as settings;
pub use fn_settings::version;
pub use sim::*;
pub use sim_tick::{Advance, Latched, Pair, Snapshot, TickClock, MAX_TICKS_PER_FRAME, SIM_HZ};
pub use tick_driver::{FrameReport, Poses, SimClock, TickDriver};