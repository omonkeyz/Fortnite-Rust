//! MUSIC HOOK: front-end music (`Fort_Entry_Music_Controller_BP`) and the loading-screen sound mix on the engine sink. Child module of the crate root: it uses the
//! private fields of `EngineSink` / `SinkLoader` / `Shared` directly. The recovered data and where it comes from: `fn_audio::music`.
//!
//! * [`MusicCue`] = the front-end tracks the Battle Royale flow has screens for. `AudioSink::set_music(Some(track))` is the controller's
//!   `Music_*_FadeIn`: the requested cue starts at 0 at once (`AudioComponent::Play(0)`, no fade-in) and the previous one fades out over `FadeOutTime` = 1.0 s
//!   (`FadeOut`); `set_music(None)` fades the current one out (the `SubGameChanged` handler).
//! * `AudioSink::set_loading_screen(on)` pushes / pops `DefaultGameData.LoadingScreenSoundMix` (`Fort_Loading_Mix`: SFX, Voice and Music to 0, 0.5 s in, 1.0 s out).
//! * The long music waves are decoded incrementally by `fn_audio::stream` workers (`Library::stream_min_seconds`), the cues are prepared on the loader thread
//!   (`SinkLoader::load_music`), the game thread only pushes commands. Requests made before the loader is done are remembered and carried out when it finishes.
//! * Music volume: the cues are in `Fort_Music_Menu_PSM` (bus Music), so the Settings > Audio Music slider (`set_volumes`) scales them, on top of Master.

use super::{EngineSink, SinkLoader};
use fn_audio::library::Library;
use fn_audio::mixer::{EngineHandle, PlayId, PlayParams};
use fn_audio::music as data;
use fn_audio::soundclass::SoundMix;
use std::sync::{Mutex, MutexGuard};

/// A front-end music track (the cue of a `Music_*_FadeIn` event of the controller).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicCue {
    /// `LoginScreen` -> `Music_Login_FadeIn`.
    Login,
    /// `AthenaLobby` (the Battle Royale lobby) -> `Music_Lobby_Athena_FadeIn`.
    LobbyAthena,
}

impl MusicCue {
    pub const ALL: [MusicCue; 2] = [MusicCue::Login, MusicCue::LobbyAthena];
    /// The SoundCue asset path.
    pub fn cue(self) -> &'static str {
        match self {
            MusicCue::Login => data::LOGIN,
            MusicCue::LobbyAthena => data::LOBBY_ATHENA,
        }
    }
}

#[derive(Default)]
struct Inner {
    mixes_ready: bool,
    cues_ready: bool,
    loading_mix: Option<SoundMix>,
    suppress_mix: Option<SoundMix>,
    killed_mix: Option<SoundMix>,
    /// A `set_music` request not carried out yet (`want` is its value; `None` = fade out).
    pending: bool,
    want: Option<MusicCue>,
    current: Option<PlayId>,
    suppress_on: bool,
    loading_wanted: bool,
    loading_on: bool,
}

/// Music state shared between the sink (game thread) and the loader (worker thread).
#[derive(Default)]
pub(crate) struct MusicState {
    inner: Mutex<Inner>,
}

impl MusicState {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
    pub(crate) fn set_music(&self, h: &EngineHandle, track: Option<MusicCue>) {
        let mut i = self.lock();
        i.want = track;
        i.pending = true;
        i.apply(h);
    }
    pub(crate) fn push_killed(&self, h: &EngineHandle) {
        let i = self.lock();
        if let Some(m) = &i.killed_mix { h.pop_mix(&m.name); h.push_mix(m.clone()); }
    }
    pub(crate) fn set_loading_screen(&self, h: &EngineHandle, on: bool) {
        let mut i = self.lock();
        i.loading_wanted = on;
        i.apply_loading(h);
    }
    /// `(cues decoded + mixes loaded, a track is playing, the loading mix is active)` (diagnostics / tests).
    pub(crate) fn status(&self) -> (bool, bool, bool) {
        let i = self.lock();
        (i.mixes_ready && i.cues_ready, i.current.is_some(), i.loading_on)
    }
}

impl Inner {
    fn apply(&mut self, h: &EngineHandle) {
        if !self.cues_ready || !self.pending {
            return;
        }
        self.pending = false;
        // controller ping-pong: the idle component plays the new sound from 0, the other one fades out over FadeOutTime
        if let Some(old) = self.current.take() {
            h.fade_play(old, 0.0, data::FADE_OUT_TIME, true);
        }
        match self.want {
            Some(t) => {
                self.current = h.play_cue(t.cue(), PlayParams::default()).ok(); // 2D: the cues override attenuation with bAttenuate = false
                self.set_suppress(h, true);
            }
            None => self.set_suppress(h, false),
        }
    }

    /// `Fort_Music_Menu_PSM` passive modifier: the "suppress normal music" mix is active while menu music plays.
    fn set_suppress(&mut self, h: &EngineHandle, on: bool) {
        if on == self.suppress_on {
            return;
        }
        let Some(m) = &self.suppress_mix else { return };
        if on {
            h.push_mix(m.clone());
        } else {
            h.pop_mix(&m.name);
        }
        self.suppress_on = on;
    }

    fn apply_loading(&mut self, h: &EngineHandle) {
        if !self.mixes_ready || self.loading_wanted == self.loading_on {
            return;
        }
        let Some(m) = &self.loading_mix else { return };
        if self.loading_wanted {
            h.push_mix(m.clone());
        } else {
            h.pop_mix(&m.name);
        }
        self.loading_on = self.loading_wanted;
    }
}

impl SinkLoader {
    /// The loading-screen mix, the menu "suppress normal music" mix and the front-end music cues (decoded incrementally by stream workers). Mixes first (the
    /// loading screen is silent from the start), then the cues. Returns what failed to load.
    pub fn load_music(&self, lib: &mut Library) -> Vec<(String, String)> {
        let mut missing = Vec::new();
        let mut mix = |lib: &mut Library, path: &str| match lib.load_mix(path) {
            Ok(Some(m)) => Some(m),
            Ok(None) => {
                missing.push((path.to_string(), "SoundMix not found".into()));
                None
            }
            Err(e) => {
                missing.push((path.to_string(), e.to_string()));
                None
            }
        };
        let (loading, suppress, killed) = (mix(lib, data::LOADING_MIX), mix(lib, data::MENU_SUPPRESS_MIX), mix(lib, data::PLAYER_KILLED_MIX));
        {
            let mut i = self.shared.music.lock();
            i.loading_mix = loading;
            i.suppress_mix = suppress;
            i.killed_mix = killed;
            i.mixes_ready = true;
            i.apply_loading(&self.handle);
        }
        let prev = lib.stream_min_seconds.replace(data::STREAM_MIN_SECONDS);
        for t in MusicCue::ALL {
            match lib.prepare_cue(&self.handle, t.cue()) {
                Ok(0) => {}
                Ok(n) => missing.push((t.cue().to_string(), format!("{n} wave(s) failed to load"))),
                Err(e) => missing.push((t.cue().to_string(), e.to_string())),
            }
        }
        lib.stream_min_seconds = prev;
        let mut i = self.shared.music.lock();
        i.cues_ready = true;
        i.apply(&self.handle);
        missing
    }
}

impl EngineSink {
    /// Front-end music (`None` = fade the current track out).
    pub fn set_music_cue(&self, track: Option<MusicCue>) {
        self.shared.music.set_music(&self.handle, track);
    }
    /// The loading-screen sound mix on / off.
    pub fn push_player_killed_mix(&self) {
        self.shared.music.push_killed(&self.handle);
    }
    pub fn set_loading_mix(&self, on: bool) {
        self.shared.music.set_loading_screen(&self.handle, on);
    }
    /// `(music ready, a track is playing, loading mix active)`.
    pub fn music_status(&self) -> (bool, bool, bool) {
        self.shared.music.status()
    }
}
