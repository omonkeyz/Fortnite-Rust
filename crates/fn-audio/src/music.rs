//! MUSIC HOOK: which music plays where in Fortnite 1.8 (Battle Royale), recovered from the install. Data only; the playback logic is `fn-audio-sink`'s `music` module.
//!
//! # What the 1.8 data says (every line was read from the packages, nothing assumed)
//!
//! **Front-end music = one Blueprint actor.** `/Game/Sounds/Fort_Audio_Blueprints/Fort_Entry_Music_Controller_BP` (placed once in `/Game/Maps/Frontend`, at
//! (128, 40960, 32) cm) owns two `AudioComponent`s `MenuMusic_A` / `MenuMusic_B` and a `FadeOutTime` of **1.0 s** (CDO). Every `Music_*_FadeIn` event sets the
//! requested cue as the current sound and then ping-pongs: `Play(0)` on the component that was idle, `FadeOut(FadeOutTime)` on the other (no fade-in; a fresh
//! start from 0 on every request, the old track fades out underneath). The events and their cues ([`ENTRY_TRACKS`]) are called by the UI:
//!
//! * `LoginScreen` (`/Game/UI/Foundation/StateLayouts/LoginScreen`): after `SetFrontEndCamera(7)` on entering the state -> `Music_Login_FadeIn` -> [`LOGIN`].
//! * `AthenaLobby` (`/Game/Athena/UI/Frontend/AthenaLobby`, the Battle Royale lobby): `OnActivated` -> `Refresh` -> `Initialize` -> `StartMusic` ->
//!   `Music_Lobby_Athena_FadeIn` -> [`LOBBY_ATHENA`].
//! * `Fort_Entry_Music_Controller_BP` binds `FortGlobalUIContext.SubGameChanged`; the handler fades **both** components out (1.0 s) when the new sub game is
//!   `ESubGame::Invalid` (2; `Campaign` = 0, `Athena` = 1, from the client's enum strings) and does nothing for 0 / 1.
//! * the other `Music_*_FadeIn` events (store, world overview, homebase, hexmap, heroes, vault and the card-pack / llama sequences) belong to Save the World screens.
//!
//! Both front-end cues play **2D** at full level: their `AttenuationOverrides` are `bAttenuate = false, bSpatialize = false` (the actor is ~85,000 cm from the
//! lobby camera, so a spatialised cue could not be heard). Both are in SoundClass `Fort_Music_Menu_PSM` (-> `Fort_11_Base_music` -> `Music` 0.5 -> `Master` 0.7, bus
//! **Music**: the Settings > Audio Music slider scales them), `VolumeMultiplier` 0.9 (lobby) / 1.0 (login), concurrency `MaxCount 1, StopOldest`, looping waves
//! (`fort_lobby_music_athena` 115.2 s OPUS 44.1 kHz stereo; `fortnite_login_screen` 76.8 s OPUS 24 kHz stereo, mixed 1.0 with `Mountain_Wind06` at 0.8).
//!
//! `Fort_Music_Menu_PSM` carries a `PassiveSoundMixModifier` (volume 0..10) for [`MENU_SUPPRESS_MIX`]: while menu music plays, `Fort_Music`, `Fort_Music_Day_PSM`
//! and `Fort_Ambient_TileMix` (+ children) go to 0 (fade 0.5 in / 0.5 out). No Athena sound is in those classes (the island ambience is `Fort_Ambient_Outdoors_Athena`).
//!
//! **Loading screens are silent.** `/Game/Balance/DefaultGameData` (`FortGameData`) has `LoadingScreenSoundMix = Fort_Loading_Mix`: a `SoundMix` that sets the
//! classes `SFX`, `Voice` and `Music` (with children) to volume **0.0**, fade in **0.5 s**, fade out **1.0 s**. The same asset holds the loading art
//! (`LoadingScreenBackground_Generic`, `LoadingScreenEABackground` = the "EARLY ACCESS" plate of the start-up screen) and `LoadingScreenTips`; there is **no** music
//! cue anywhere in the loading screen data. The start-up loading screen and the level-travel loading screen both use it (native code pushes the mix; the push /
//! pop moments are native, not in the install).
//!
//! **Pre-game island and bus (`Athena_Terrain_Audio` sub level, `B_StockBattleBus`).** Not music-class sounds: both cue families are `Fort_Gadgets` (-> SFX bus,
//! 0.28 chain volume, `VolumeMultiplier` 3), 3D and attenuated.
//! * `BP_Bus_Lobby_SoundController` (placed at (-116796, -108932, 4472) cm): `BeginPlay` spawns `LoopingSound` ([`BUS_LOBBY_ENGINE`]) and `LoopingMusic`
//!   ([`BUS_LOBBY_MUSIC`]) attached, binds `Athena_GameState.GamePhaseStepChanged`; `Tick` (while `bGettingReady`) sets both volume multipliers to
//!   `Curve_BusGetReadyFadeValue(MapRangeClamped(now, GetReadyGameTime, WarmupCountdownEndTime, 0, 1))` (curve: 1.0 until 0.7, linear to 0.0 at 1.0) and plays
//!   `Athena_Bus_Flying_Transition_Cue` (2D) once when that fraction passes 0.8; phase step 3 (`GetReady`) spawns [`BUS_IGNITION`] attached and waits
//!   `IgnitionTime` = 4.0 s; step 5 (`BusFlying`) disables the tick (`EAthenaGamePhaseStep`: None 0, Setup 1, Warmup 2, GetReady 3, BusLocked 4, BusFlying 5,
//!   StormForming 6, StormHolding 7, StormShrinking 8, EndGame 9).
//! * `B_StockBattleBus` (`Start Audio`): spawns `LoopingSound` ([`BUS_FLYING_LOOP`]) and `LoopingMusic` ([`BUS_FLYING_MUSIC`]) attached to the bus, both
//!   `FadeIn(2.0 s, volume 1)`.
//!
//! **In-match music stingers.** None are bound for Battle Royale in the 1.8 data: `PlayerUIFeedback` (`FortUIFeedbackBank`) has no audio for `PvPVictory`,
//! `PvPLocalPlayerKilled`, `PvPEnemyKilled...`; its music entries (`Mission_*`, `Objective_*`, `Bonus_*`, `Horde_*`, `ZoneCompleteSuccess` ->
//! `fortnite_victory_music_loop_cue`) are Save the World. `/Game/Sounds/PVP/Music_Death_Stinger_Cue` is referenced by no package and no client string (orphan).
//! `DefaultGameData.MusicManager` (`/Game/Blueprints/MusicManager`: combat-intensity layers, `AmbientMusic/DayTime..`) is the Save the World manager.
//!
//! **Config (decrypted 2026-10-06):** there is no config-driven startup movie / sound: no `StartupMovies` in any 1.8 ini and DefaultEngine.ini line 522 disables the
//! WindowsMoviePlayer plugin. The native push / pop timing of the
//! loading mix and the native choice of the sub game at level travel are code, not data.

/// `Fort_Entry_Music_Controller_BP` CDO `FadeOutTime`: the old track's `AudioComponent::FadeOut` time (seconds).
pub const FADE_OUT_TIME: f32 = 1.0;

/// `LoginScreen` -> `Music_Login_FadeIn` (`Sound_Current_Music_A = fortnite_login_music_loop_Cue`).
pub const LOGIN: &str = "/Game/Sounds/Fort_Music/Menu/fortnite_login_music_loop_Cue";
/// `AthenaLobby.StartMusic` -> `Music_Lobby_Athena_FadeIn` (the `Sound_Current_Music_A` literal of the ubergraph entry 10).
pub const LOBBY_ATHENA: &str = "/Game/Sounds/Fort_Music/Menu/fortnite_lobby_music_athena_Cue";

/// `DefaultGameData.LoadingScreenSoundMix`.
pub const LOADING_MIX: &str = "/Game/Sounds/FortSoundClassesAndModes/Fort_Loading_Mix";
/// `Fort_Music_Menu_PSM.PassiveSoundMixModifiers[0]` (min volume 0, max volume 10).
pub const PLAYER_KILLED_MIX: &str = "/Game/Sounds/FortSoundClassesAndModes/Fort_PlayerKilled_Athena_MixMod";
pub const MENU_SUPPRESS_MIX: &str = "/Game/Sounds/FortSoundClassesAndModes/Fort_Suppress_NormalMusic_MixMod";
/// The SoundClass of every front-end music cue.
pub const MENU_MUSIC_CLASS: &str = "Fort_Music_Menu_PSM";

/// `BP_Bus_Lobby_SoundController.LoopingMusic` (CDO).
pub const BUS_LOBBY_MUSIC: &str = "/Game/Athena/Sounds/Vehicles/Athena_Bus_Music_Lobby_Loop_Cue";
/// `BP_Bus_Lobby_SoundController.LoopingSound` (CDO).
pub const BUS_LOBBY_ENGINE: &str = "/Game/Athena/Sounds/Vehicles/Athena_Bus_Engine_Idle_Cue";
/// `BP_Bus_Lobby_SoundController.IgnitionSound` (CDO; `IgnitionTime` 4.0 s).
pub const BUS_IGNITION: &str = "/Game/Athena/Sounds/Vehicles/Athena_Bus_Ignition_Cue";
/// Played 2D by `BP_Bus_Lobby_SoundController.ReceiveTick` once the get-ready fraction passes 0.8.
pub const BUS_TRANSITION: &str = "/Game/Athena/Sounds/Vehicles/Athena_Bus_Flying_Transition_Cue";
/// `B_StockBattleBus.LoopingMusic` (CDO).
pub const BUS_FLYING_MUSIC: &str = "/Game/Athena/Sounds/Vehicles/Athena_Bus_Music_Loop_Cue";
/// `B_StockBattleBus.LoopingSound` (CDO).
pub const BUS_FLYING_LOOP: &str = "/Game/Athena/Sounds/Vehicles/Athena_Bus_Flying_Loop_Cue";

/// A front-end track of `Fort_Entry_Music_Controller_BP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Track {
    /// The controller event that starts it.
    pub event: &'static str,
    pub cue: &'static str,
    /// Which screen calls the event (recovered from the UI bytecode) or why it is not wired here.
    pub caller: &'static str,
}

/// Every `Music_*_FadeIn` event of the controller with its cue (the `Sound_Current_Music_A = obj ...` literal of the ubergraph entry of that event).
pub const ENTRY_TRACKS: &[Track] = &[
    Track { event: "Music_Login_FadeIn", cue: LOGIN, caller: "LoginScreen (state enter)" },
    Track { event: "Music_Lobby_Athena_FadeIn", cue: LOBBY_ATHENA, caller: "AthenaLobby.StartMusic (OnActivated)" },
    Track { event: "Music_Store_FadeIn", cue: "/Game/Sounds/Fort_Music/Menu/fortnite_store_music_loop_Cue", caller: "Save the World store" },
    Track { event: "Music_Lobby_FadeIn", cue: "/Game/Sounds/Fort_Music/Menu/fortnite_lobby_music_loop_Cue", caller: "Save the World lobby" },
    Track { event: "Music_WorldOverview_FadeIn", cue: "/Game/Sounds/Fort_Music/Menu/fortnite_world_music_loop_Cue", caller: "Save the World world map" },
    Track { event: "Music_HomeBase_FadeIn", cue: "/Game/Sounds/Fort_Music/Menu/fortnite_homebase_music_loop_alt_Cue", caller: "Save the World homebase" },
    Track { event: "Music_Hexmap_FadeIn", cue: "/Game/Sounds/Fort_Music/Menu/fortnite_hexmap_music_loop_Cue", caller: "Save the World hex map" },
    Track { event: "Music_Heroes_FadeIn", cue: "/Game/Sounds/Fort_Music/Menu/fortnite_heroes_music_loop_Cue", caller: "Save the World heroes" },
    Track { event: "Music_Vault_FadeIn", cue: "/Game/Sounds/Fort_Music/Menu/fortnite_vault_music_loop_Cue", caller: "Save the World vault" },
];

/// Cues of the Battle Royale music flow that go into the audio pack (front-end tracks + the bus / pre-game island sounds).
pub const PACK_CUES: &[&str] = &[LOGIN, LOBBY_ATHENA, BUS_LOBBY_MUSIC, BUS_LOBBY_ENGINE, BUS_IGNITION, BUS_TRANSITION, BUS_FLYING_MUSIC, BUS_FLYING_LOOP];

/// Waves of at least this many seconds are decoded incrementally by a worker (`Library::stream_min_seconds`) instead of whole: the two front-end tracks are 77 s and
/// 115 s (a whole decode of the lobby track is ~14 s in release); the bus loops are 7 s and decode whole.
pub const STREAM_MIN_SECONDS: f64 = 20.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_consistent() {
        assert!(ENTRY_TRACKS.iter().any(|t| t.cue == LOGIN) && ENTRY_TRACKS.iter().any(|t| t.cue == LOBBY_ATHENA));
        for t in ENTRY_TRACKS {
            assert!(t.cue.starts_with("/Game/Sounds/Fort_Music/Menu/") && t.event.starts_with("Music_") && t.event.ends_with("FadeIn"), "{t:?}");
        }
        let mut s = PACK_CUES.to_vec();
        s.sort();
        s.dedup();
        assert_eq!(s.len(), PACK_CUES.len());
        assert!(PACK_CUES.iter().all(|c| c.starts_with("/Game/")));
    }
}
