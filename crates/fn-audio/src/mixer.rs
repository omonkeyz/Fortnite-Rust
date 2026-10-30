//! The mixer: SoundCue expansion into voices + block rendering (stereo f32), driven by a lock-free command queue.
//!
//! * [`EngineHandle`] is the thread-safe front end (clone it anywhere): `play_cue`, `stop*`, `set_bus_volume`,
//!   `set_listener`, `add_wave`/`add_cue`, `push_mix`. It only pushes [`Command`]s on a bounded `ArrayQueue`.
//! * [`Mixer`] owns all audio state and is run either offline ([`AudioEngine::render_offline`], deterministic for a
//!   seed) or from the cpal callback (`device` module). It drains the queue every 64-frame block.
//! * Per voice and block: distance attenuation from the cue's recovered `SoundAttenuation` (see [`crate::attenuation`]),
//!   equal-power pan by listener-relative azimuth blended by `OmniRadius`, optional low-pass (`bAttenuateWithLPF`),
//!   bus volumes (Master/Music/SFX/Voice/VoiceChat sliders x the asset class chain), SoundMix adjusters, envelope /
//!   cross-fade gains; linear gain ramps across the block avoid zipper noise.

use crate::attenuation::Attenuation;
use crate::cue::{CueNode, FadeRange, NodeKind, ResolutionRule, SoundCue};
use crate::error::{AudioError, Result};
use crate::soundclass::{Bus, ClassInfo, SoundMix};
use crate::stream::{StreamCursor, StreamedWave};
use crate::wave::DecodedWave;
use crossbeam_queue::ArrayQueue;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Frames per render block (command latency + gain ramp length).
pub const BLOCK: usize = 64;
/// Default voice limit.
pub const DEFAULT_MAX_VOICES: usize = 48;
const FOREVER: u64 = u64::MAX / 4;

/// World-space vector. Units are UE centimetres (the sink converts from metres); right-handed, y-up is assumed by
/// the listener basis (`right = forward x up`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3 { x, y, z }
    }
    pub fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }
    pub fn normalized(self) -> Vec3 {
        let l = self.length();
        if l > 1e-6 {
            Vec3::new(self.x / l, self.y / l, self.z / l)
        } else {
            Vec3::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Listener {
    pub position: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
}

impl Default for Listener {
    fn default() -> Self {
        Listener { position: Vec3::default(), forward: Vec3::new(0.0, 0.0, -1.0), up: Vec3::new(0.0, 1.0, 0.0) }
    }
}

/// A decoded wave plus the per-asset `Volume` / `Pitch` properties of the SoundWave.
#[derive(Clone, Debug)]
pub struct LoadedWave {
    /// The decoded samples; for a streamed wave only the format fields (rate, channels, loop points) are set and
    /// `samples` is empty (see `stream`).
    pub pcm: DecodedWave,
    pub volume: f32,
    pub pitch: f32,
    /// Incremental decode of a long looping wave (ambience, music): see [`crate::stream`].
    pub stream: Option<Arc<StreamedWave>>,
    pub pcm16: Vec<i16>,
    pub lazy: Option<Arc<crate::soundwave::SoundWaveAsset>>,
}

impl LoadedWave {
    pub fn new(pcm: DecodedWave) -> LoadedWave {
        LoadedWave { pcm, volume: 1.0, pitch: 1.0, stream: None, pcm16: Vec::new(), lazy: None }
    }
    /// A wave decoded on the fly by `stream` (no samples held).
    pub fn streamed(stream: Arc<StreamedWave>, looping_asset: bool) -> LoadedWave {
        let mut pcm = DecodedWave::new(Vec::new(), stream.sample_rate(), stream.channels());
        if looping_asset {
            pcm.loop_start = Some(0);
            pcm.loop_end = Some(stream.frames());
        }
        LoadedWave { pcm, volume: 1.0, pitch: 1.0, stream: Some(stream), pcm16: Vec::new(), lazy: None }
    }
    /// Length in frames (of the stream when streamed).
    pub fn frames(&self) -> usize {
        match &self.stream {
            Some(s) => s.frames() as usize,
            None => if self.pcm16.is_empty() { self.pcm.frames() } else { self.pcm16.len() / (self.pcm.channels.max(1) as usize) },
        }
    }
    pub fn encoded(asset: crate::soundwave::SoundWaveAsset, volume: f32, pitch: f32) -> LoadedWave {
        LoadedWave { pcm: DecodedWave::new(Vec::new(), asset.info.sample_rate.unwrap_or(44100), asset.info.channels.unwrap_or(1)), volume, pitch, stream: None, pcm16: Vec::new(), lazy: Some(Arc::new(asset)) }
    }
    pub fn compact(mut self) -> LoadedWave {
        if self.stream.is_none() && !self.pcm.samples.is_empty() {
            self.pcm16 = self.pcm.samples.iter().map(|s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16).collect();
            self.pcm.samples = Vec::new();
        }
        self
    }
}

#[derive(Clone, Debug)]
pub struct PlayParams {
    /// World position (cm). `None` = 2D (no distance attenuation or panning).
    pub position: Option<Vec3>,
    pub volume: f32,
    pub pitch: f32,
    /// Owner id for `bLimitToOwner` concurrency rules.
    pub owner: u64,
    /// Integer parameters for `SoundNodeSwitch` (e.g. `FS_Surfaces`).
    pub ints: Vec<(String, i32)>,
    /// Float parameters for `SoundNodeParamCrossFade` (e.g. `FallingPercent`).
    pub floats: Vec<(String, f32)>,
}

impl Default for PlayParams {
    fn default() -> Self {
        PlayParams { position: None, volume: 1.0, pitch: 1.0, owner: 0, ints: Vec::new(), floats: Vec::new() }
    }
}

pub type PlayId = u64;

pub enum Command {
    AddWave(String, Arc<LoadedWave>),
    AddCue(String, Arc<SoundCue>),
    Play(PlayId, String, PlayParams),
    Stop(PlayId),
    StopCue(String),
    StopAll,
    SetBus(Bus, f32),
    Listener(Listener),
    Suspend(bool),
    PushMix(Arc<MixDef>),
    PopMix(String),
    SetQuality(usize),
    SetMaxVoices(usize),
    /// MUSIC HOOK: ramp the gain of one play (every voice it started) to `target` over `secs` (UE `AudioComponent::FadeOut` / `AdjustVolume`);
    /// the play is stopped once it is silent when the flag is set.
    Control(PlayId, f32, f32, bool),
    /// AMBIENCE HOOK: live adjustment of a play (gain ramp from / to, position, attenuation override, attenuation distance, low-pass); see `playadj`.
    Adjust(PlayId, crate::playadj::Adjust),
}

/// MUSIC HOOK: per-play gain ramp state (see [`Command::Control`]).
#[derive(Clone, Copy, Debug)]
struct PlayCtl {
    id: PlayId,
    cur: f32,
    target: f32,
    /// Gain units per second (`f32::INFINITY` = immediate).
    rate: f32,
    stop: bool,
}

/// A SoundMix with class chains resolved (`chain` = lower-case class names from the class up to the root).
#[derive(Clone, Debug)]
pub struct MixDef {
    pub mix: SoundMix,
}

/// Thread-safe command producer (cheap to clone).
#[derive(Clone)]
pub struct EngineHandle {
    uid: usize,
    queue: Arc<ArrayQueue<Command>>,
    next_id: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

/// Normalise a package/object path to the key used by the wave and cue tables.
pub fn asset_key(path: &str) -> String {
    path.split('.').next().unwrap_or(path).to_ascii_lowercase()
}

impl EngineHandle {
    fn push(&self, c: Command) -> Result<()> {
        self.queue.push(c).map_err(|_| {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            AudioError::QueueFull
        })
    }
    pub fn add_wave(&self, path: &str, wave: LoadedWave) -> Result<()> {
        self.push(Command::AddWave(asset_key(path), Arc::new(wave)))
    }
    pub fn add_wave_arc(&self, path: &str, wave: Arc<LoadedWave>) -> Result<()> {
        self.push(Command::AddWave(asset_key(path), wave))
    }
    pub fn add_cue(&self, path: &str, cue: Arc<SoundCue>) -> Result<()> {
        self.push(Command::AddCue(asset_key(path), cue))
    }
    /// Start a cue. Returns the play id (use it with [`stop`](Self::stop)). Unknown cues are ignored by the mixer.
    pub fn play_cue(&self, path: &str, params: PlayParams) -> Result<PlayId> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.push(Command::Play(id, asset_key(path), params))?;
        Ok(id)
    }
    pub fn stop(&self, id: PlayId) {
        let _ = self.push(Command::Stop(id));
    }
    pub fn stop_cue(&self, path: &str) {
        let _ = self.push(Command::StopCue(asset_key(path)));
    }
    pub fn stop_all(&self) {
        let _ = self.push(Command::StopAll);
    }
    /// Set a user-facing bus volume (0..=4; the Settings sliders are 0..1).
    pub fn set_bus_volume(&self, bus: Bus, v: f32) {
        let _ = self.push(Command::SetBus(bus, v.clamp(0.0, 4.0)));
    }
    pub fn set_listener(&self, position: Vec3, forward: Vec3, up: Vec3) {
        let _ = self.push(Command::Listener(Listener { position, forward, up }));
    }
    /// Mute / unmute (focus loss); voices keep running silently.
    pub fn set_suspended(&self, s: bool) {
        let _ = self.push(Command::Suspend(s));
    }
    pub fn push_mix(&self, mix: SoundMix) {
        let _ = self.push(Command::PushMix(Arc::new(MixDef { mix })));
    }
    pub fn pop_mix(&self, name: &str) {
        let _ = self.push(Command::PopMix(name.to_string()));
    }
    /// Audio quality level used by `SoundNodeQualityLevel` (child index; 0 default).
    pub fn set_quality(&self, level: usize) {
        let _ = self.push(Command::SetQuality(level));
    }
    pub fn set_max_voices(&self, n: usize) {
        let _ = self.push(Command::SetMaxVoices(n.max(1)));
    }
    /// MUSIC HOOK: ramp the gain of the play `id` to `target` (0..=1 multiplier on top of everything else) over `secs`; with `stop_when_silent`
    /// the play ends when it reaches 0 (`AudioComponent::FadeOut`).
    pub fn fade_play(&self, id: PlayId, target: f32, secs: f32, stop_when_silent: bool) {
        let _ = self.push(Command::Control(id, target.clamp(0.0, 1.0), secs.max(0.0), stop_when_silent));
    }
    /// AMBIENCE HOOK: adjust a running play (UE `UAudioComponent` FadeIn / FadeOut / SetWorldLocation / attenuation override / SetLowPassFilterFrequency).
    pub fn adjust_play(&self, id: PlayId, a: crate::playadj::Adjust) {
        let _ = self.push(Command::Adjust(id, a));
    }
    /// Process-unique identity of the engine this handle talks to (clones share it).
    pub fn id(&self) -> usize {
        self.uid
    }
    /// Commands dropped because the queue was full.
    pub fn dropped_commands(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[derive(Clone)]
struct Ctx {
    volume: f32,
    pitch: f32,
    atten: Option<Arc<Attenuation>>,
    class: Arc<ClassInfo>,
    fades: Vec<FadeRange>,
    env: Option<Arc<Envelope>>,
    params: Vec<(String, FadeRange, f32)>,
}

#[derive(Debug)]
struct Envelope {
    volume: crate::curve::Curve,
    pitch: crate::curve::Curve,
    looping: bool,
    loop_start: f32,
    loop_end: f32,
}

impl Envelope {
    fn at(&self, t: f32) -> (f32, f32) {
        let mut t = t;
        if self.looping && self.loop_end > self.loop_start && t > self.loop_end {
            t = self.loop_start + (t - self.loop_start) % (self.loop_end - self.loop_start);
        }
        (self.volume.eval(t, 1.0), self.pitch.eval(t, 1.0))
    }
}

struct PlayInfo {
    id: PlayId,
    cue_key: u64,
    owner: u64,
    group: u64,
    priority: f32,
    position: Option<Vec3>,
    ints: Vec<(String, i32)>,
    floats: Vec<(String, f32)>,
    concurrency_scale: f32,
}

struct Voice {
    play: PlayId,
    cue_key: u64,
    group: u64,
    owner: u64,
    priority: f32,
    wave: Arc<LoadedWave>,
    pos: f64,
    delay: u64,
    rate: f64,
    looping: bool,
    /// Playhead into the wave's [`StreamedWave`] (`pos` is then the position in the cursor's sequence).
    stream: Option<StreamCursor>,
    base_gain: f32,
    class: Arc<ClassInfo>,
    atten: Option<Arc<Attenuation>>,
    position: Option<Vec3>,
    fades: Vec<FadeRange>,
    env: Option<Arc<Envelope>>,
    params: Vec<(String, FadeRange, f32)>,
    gl: f32,
    gr: f32,
    lpf: [f32; 2],
    age: u64,
    fade_out: Option<u32>,
    started: bool,
    done: bool,
    last_gain: f32,
    pending: Option<usize>,
}

struct Scheduled {
    at: u64,
    cue: Arc<SoundCue>,
    path: Vec<usize>,
    ctx: Ctx,
    play: Arc<PlayInfoOwned>,
    remaining: Option<u32>,
}

#[derive(Clone)]
struct PlayInfoOwned {
    id: PlayId,
    cue_key: u64,
    owner: u64,
    group: u64,
    priority: f32,
    position: Option<Vec3>,
    ints: Vec<(String, i32)>,
    floats: Vec<(String, f32)>,
    concurrency_scale: f32,
}

impl PlayInfoOwned {
    fn as_info(&self) -> PlayInfo {
        PlayInfo { id: self.id, cue_key: self.cue_key, owner: self.owner, group: self.group, priority: self.priority, position: self.position, ints: self.ints.clone(), floats: self.floats.clone(), concurrency_scale: self.concurrency_scale }
    }
}

struct ActiveMix {
    def: Arc<MixDef>,
    age: f32,
    popped_at: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MixerStats {
    pub plays: u64,
    pub plays_prevented: u64,
    pub voices_started: u64,
    pub voices_stolen: u64,
    pub missing_waves: u64,
    pub unknown_cues: u64,
    /// Streamed voices that could not get a playhead (every slot of the wave busy).
    pub stream_starved: u64,
}

fn hash64(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub struct Mixer {
    queue: Arc<ArrayQueue<Command>>,
    out_rate: u32,
    waves: HashMap<String, Arc<LoadedWave>>,
    cues: HashMap<String, Arc<SoundCue>>,
    bus: [f32; 5],
    listener: Listener,
    voices: Vec<Voice>,
    scheduled: Vec<Scheduled>,
    mixes: Vec<ActiveMix>,
    random_used: HashMap<(u64, u64), u64>,
    rng: u64,
    max_voices: usize,
    clock: u64,
    suspended: bool,
    suspend_gain: f32,
    quality: usize,
    /// Offline rendering: wait for the stream workers instead of stalling a voice (deterministic output).
    wait_streams: bool,
    /// MUSIC HOOK: active per-play gain ramps.
    ctl: Vec<PlayCtl>,
    /// AMBIENCE HOOK: per-play live adjustments (`playadj`).
    adj: Vec<crate::playadj::PlayAdj>,
    decoded: HashMap<usize, (Arc<LoadedWave>, u64)>,
    decoded_bytes: usize,
    decoding: std::collections::HashSet<usize>,
    decode_tx: Option<std::sync::mpsc::Sender<(usize, Arc<crate::soundwave::SoundWaveAsset>, f32, f32)>>,
    decode_done: Arc<std::sync::Mutex<Vec<(usize, Option<Arc<LoadedWave>>)>>>,
    pub stats: MixerStats,
}

const DECODED_BUDGET: usize = 48 << 20;

/// Create a connected handle + mixer.
pub fn engine_pair(out_rate: u32) -> (EngineHandle, Mixer) {
    let queue = Arc::new(ArrayQueue::new(8192));
    static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);
    let handle = EngineHandle { uid: NEXT_ENGINE.fetch_add(1, Ordering::Relaxed) as usize, queue: queue.clone(), next_id: Arc::new(AtomicU64::new(1)), dropped: Arc::new(AtomicU64::new(0)) };
    (handle, Mixer::new(queue, out_rate))
}

impl Mixer {
    fn new(queue: Arc<ArrayQueue<Command>>, out_rate: u32) -> Mixer {
        Mixer {
            queue,
            out_rate: out_rate.max(1),
            waves: HashMap::new(),
            cues: HashMap::new(),
            bus: [1.0; 5],
            listener: Listener::default(),
            voices: Vec::new(),
            scheduled: Vec::new(),
            mixes: Vec::new(),
            random_used: HashMap::new(),
            rng: 0x9E3779B97F4A7C15,
            max_voices: DEFAULT_MAX_VOICES,
            clock: 0,
            decoded: HashMap::new(),
            decoded_bytes: 0,
            decoding: std::collections::HashSet::new(),
            decode_tx: None,
            decode_done: Arc::new(std::sync::Mutex::new(Vec::new())),
            suspended: false,
            suspend_gain: 1.0,
            quality: 0,
            wait_streams: false,
            ctl: Vec::new(),
            adj: Vec::new(), // AMBIENCE HOOK
            stats: MixerStats::default(),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.out_rate
    }
    pub fn set_seed(&mut self, seed: u64) {
        self.rng = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
    }
    pub fn voice_count(&self) -> usize {
        self.voices.iter().filter(|v| !v.done).count()
    }
    pub fn bus_volume(&self, b: Bus) -> f32 {
        self.bus[b.index()]
    }

    fn rand(&mut self) -> f32 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        ((x.wrapping_mul(0x2545F4914F6CDD1D) >> 40) as f32) / (1u64 << 24) as f32
    }
    fn rand_range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.rand()
    }

    // ------------------------------------------------------------------ commands

    fn drain_commands(&mut self) {
        while let Some(c) = self.queue.pop() {
            match c {
                Command::AddWave(k, w) => {
                    self.waves.insert(k, w);
                }
                Command::AddCue(k, c) => {
                    self.cues.insert(k, c);
                }
                Command::Play(id, name, params) => self.start_play(id, &name, params),
                Command::Stop(id) => self.stop_play(id),
                Command::StopCue(name) => {
                    let key = hash64(&name);
                    let ids: Vec<PlayId> = self.voices.iter().filter(|v| v.cue_key == key).map(|v| v.play).collect();
                    for id in ids {
                        self.stop_play(id);
                    }
                }
                Command::StopAll => {
                    for v in &mut self.voices {
                        v.fade_out.get_or_insert(256);
                    }
                    self.scheduled.clear();
                }
                Command::SetBus(b, v) => self.bus[b.index()] = v,
                Command::Listener(l) => self.listener = l,
                Command::Suspend(s) => self.suspended = s,
                Command::PushMix(d) => {
                    // re-pushing an active mix restarts it (UE keeps one instance per SoundMix)
                    self.mixes.retain(|m| m.def.mix.name != d.mix.name);
                    self.mixes.push(ActiveMix { def: d, age: 0.0, popped_at: None });
                }
                Command::PopMix(n) => {
                    for m in &mut self.mixes {
                        if m.def.mix.name == n && m.popped_at.is_none() {
                            m.popped_at = Some(m.age);
                        }
                    }
                }
                Command::SetQuality(q) => self.quality = q,
                Command::SetMaxVoices(n) => self.max_voices = n,
                Command::Adjust(id, a) => crate::playadj::apply(&mut self.adj, id, a), // AMBIENCE HOOK
                Command::Control(id, target, secs, stop) => {
                    // MUSIC HOOK
                    let i = match self.ctl.iter().position(|c| c.id == id) {
                        Some(i) => i,
                        None => {
                            self.ctl.push(PlayCtl { id, cur: 1.0, target: 1.0, rate: 0.0, stop: false });
                            self.ctl.len() - 1
                        }
                    };
                    let c = &mut self.ctl[i];
                    c.target = target;
                    c.stop = stop;
                    c.rate = if secs > 0.0 { (target - c.cur).abs() / secs } else { f32::INFINITY };
                }
            }
        }
    }

    fn stop_play(&mut self, id: PlayId) {
        for v in &mut self.voices {
            if v.play == id {
                v.fade_out.get_or_insert(256);
            }
        }
        self.scheduled.retain(|s| s.play.id != id);
    }

    fn start_play(&mut self, id: PlayId, name: &str, params: PlayParams) {
        let Some(cue) = self.cues.get(name).cloned() else {
            self.stats.unknown_cues += 1;
            return;
        };
        // concurrency
        let group_name = cue.concurrency.as_ref().map(|c| c.group.clone()).unwrap_or_else(|| name.to_string());
        let group = hash64(&if cue.concurrency.is_some() { format!("conc:{group_name}") } else { name.to_string() });
        let mut concurrency_scale = 1.0;
        if let Some(c) = &cue.concurrency {
            concurrency_scale = c.volume_scale;
            // distinct active plays in the group
            let mut plays: Vec<(PlayId, u64, Option<Vec3>)> = Vec::new();
            for v in self.voices.iter().filter(|v| !v.done && v.group == group && v.fade_out.is_none()) {
                if c.limit_to_owner && v.owner != params.owner {
                    continue;
                }
                if !plays.iter().any(|p| p.0 == v.play) {
                    plays.push((v.play, v.age, v.position));
                }
            }
            if plays.len() as u32 >= c.max_count {
                let lis = self.listener.position;
                let dist = |p: &Option<Vec3>| p.map(|p| p.sub(lis).length()).unwrap_or(0.0);
                let victim = match c.rule {
                    ResolutionRule::PreventNew | ResolutionRule::StopLowestPriorityThenPreventNew => None,
                    ResolutionRule::StopFarthestThenPreventNew | ResolutionRule::StopFarthestThenOldest => {
                        let far = plays.iter().max_by(|a, b| dist(&a.2).partial_cmp(&dist(&b.2)).unwrap_or(std::cmp::Ordering::Equal)).copied();
                        match far {
                            Some(f) if dist(&params.position) <= dist(&f.2) => Some(f.0),
                            Some(f) if c.rule == ResolutionRule::StopFarthestThenOldest => plays.iter().max_by_key(|p| p.1).map(|p| p.0).or(Some(f.0)),
                            _ => None,
                        }
                    }
                    _ => plays.iter().max_by_key(|p| p.1).map(|p| p.0), // StopOldest / StopQuietest / StopLowestPriority: oldest play
                };
                match victim {
                    Some(v) => {
                        self.stop_play(v);
                        self.stats.voices_stolen += 1;
                    }
                    None => {
                        self.stats.plays_prevented += 1;
                        return;
                    }
                }
            }
        }
        self.stats.plays += 1;
        let play = PlayInfo { id, cue_key: hash64(name), owner: params.owner, group, priority: cue.priority, position: params.position, ints: params.ints, floats: params.floats, concurrency_scale };
        let ctx = Ctx {
            volume: params.volume * cue.volume_multiplier,
            pitch: params.pitch * cue.pitch_multiplier,
            atten: cue.attenuation.clone(),
            class: Arc::new(cue.class.clone()),
            fades: Vec::new(),
            env: None,
            params: Vec::new(),
        };
        if let Some(root) = &cue.root {
            let mut path = Vec::new();
            self.expand(&cue, root, &mut path, &ctx, 0, &play);
        }
    }

    // ------------------------------------------------------------------ expansion

    fn node_at<'a>(root: &'a CueNode, path: &[usize]) -> Option<&'a CueNode> {
        let mut n = root;
        for &i in path {
            n = n.children.get(i)?;
        }
        Some(n)
    }

    fn param_float(play: &PlayInfo, name: &str) -> f32 {
        play.floats.iter().find(|(n, _)| n == name).map(|(_, v)| *v).unwrap_or(0.0)
    }

    /// Expand `node` (reached via `path` from the root) into voices; returns the end frame (relative to now) of the
    /// node's output.
    fn expand(&mut self, cue: &Arc<SoundCue>, node: &CueNode, path: &mut Vec<usize>, ctx: &Ctx, delay: u64, play: &PlayInfo) -> u64 {
        match &node.kind {
            NodeKind::WavePlayer { wave, looping } => {
                let Some(w) = wave.as_ref().and_then(|p| self.waves.get(&crate::mixer::asset_key(p))).cloned() else {
                    self.stats.missing_waves += 1;
                    return delay;
                };
                self.spawn_voice(w, *looping, ctx, delay, play)
            }
            NodeKind::Dialogue { dialogue_wave } => {
                let Some(w) = dialogue_wave.as_ref().and_then(|p| self.waves.get(&asset_key(p))).cloned() else {
                    self.stats.missing_waves += 1;
                    return delay;
                };
                self.spawn_voice(w, false, ctx, delay, play)
            }
            NodeKind::Random { weights, without_replacement } => {
                let n = node.children.len();
                if n == 0 {
                    return delay;
                }
                let key = (hash64(&cue.path), path.iter().fold(7u64, |h, &i| h.wrapping_mul(31).wrapping_add(i as u64 + 1)));
                let mut mask = if *without_replacement { *self.random_used.get(&key).unwrap_or(&0) } else { 0 };
                let w = |i: usize| -> f32 { weights.get(i).copied().unwrap_or(1.0).max(0.0) };
                let avail = |mask: u64, i: usize| !*without_replacement || i >= 64 || mask & (1 << i) == 0;
                let mut total: f32 = (0..n).filter(|&i| avail(mask, i)).map(w).sum();
                if total <= 0.0 {
                    mask = 0;
                    total = (0..n).map(w).sum();
                }
                let mut pick = n - 1;
                if total > 0.0 {
                    let mut r = self.rand() * total;
                    for i in 0..n {
                        if !avail(mask, i) {
                            continue;
                        }
                        r -= w(i);
                        if r <= 0.0 {
                            pick = i;
                            break;
                        }
                    }
                } else {
                    pick = ((self.rand() * n as f32) as usize).min(n - 1);
                }
                if *without_replacement && pick < 64 {
                    self.random_used.insert(key, mask | (1 << pick));
                }
                path.push(pick);
                let e = self.expand(cue, &node.children[pick], path, ctx, delay, play);
                path.pop();
                e
            }
            NodeKind::Mixer { input_volume } => self.expand_all(cue, node, path, ctx, delay, play, Some(input_volume)),
            NodeKind::GroupControl { .. } | NodeKind::Unknown(_) | NodeKind::Doppler { .. } | NodeKind::ModulatorContinuous => self.expand_all(cue, node, path, ctx, delay, play, None),
            NodeKind::Concatenator { input_volume } => {
                let mut at = delay;
                for (i, c) in node.children.iter().enumerate() {
                    let mut cc = ctx.clone();
                    cc.volume *= input_volume.get(i).copied().unwrap_or(1.0);
                    path.push(i);
                    at = self.expand(cue, c, path, &cc, at, play);
                    path.pop();
                }
                at
            }
            NodeKind::Modulator { pitch, volume } => {
                let mut cc = ctx.clone();
                cc.volume *= self.rand_range(volume.0.min(volume.1), volume.0.max(volume.1));
                cc.pitch *= self.rand_range(pitch.0.min(pitch.1), pitch.0.max(pitch.1));
                self.expand_all(cue, node, path, &cc, delay, play, None)
            }
            NodeKind::Attenuation(a) => {
                let mut cc = ctx.clone();
                if a.is_some() {
                    cc.atten = a.clone();
                }
                self.expand_all(cue, node, path, &cc, delay, play, None)
            }
            NodeKind::SoundClass { info, .. } => {
                let mut cc = ctx.clone();
                if let Some(i) = info {
                    cc.class = Arc::new(i.clone());
                }
                self.expand_all(cue, node, path, &cc, delay, play, None)
            }
            NodeKind::Enveloper { volume, pitch, looping, loop_start, loop_end } => {
                let mut cc = ctx.clone();
                cc.env = Some(Arc::new(Envelope { volume: volume.clone(), pitch: pitch.clone(), looping: *looping, loop_start: *loop_start, loop_end: *loop_end }));
                self.expand_all(cue, node, path, &cc, delay, play, None)
            }
            NodeKind::Delay { min, max } => {
                let secs = self.rand_range(min.min(*max), min.max(*max)).max(0.0);
                let d = (secs * self.out_rate as f32) as u64;
                self.expand_all(cue, node, path, ctx, delay + d, play, None)
            }
            NodeKind::Switch { param } | NodeKind::Branch { param } => {
                if node.children.is_empty() {
                    return delay;
                }
                let set = play.ints.iter().find(|(n, _)| n.eq_ignore_ascii_case(param)).map(|(_, v)| *v);
                let i = match (&node.kind, set) {
                    (NodeKind::Switch { .. }, Some(v)) if v >= 0 && (v as usize + 1) < node.children.len() => v as usize + 1,
                    (NodeKind::Switch { .. }, _) => 0,
                    (_, Some(v)) => if v != 0 { 0 } else { 1 },
                    (_, None) => 2,
                };
                if i >= node.children.len() {
                    return delay;
                }
                path.push(i);
                let e = self.expand(cue, &node.children[i], path, ctx, delay, play);
                path.pop();
                e
            }
            NodeKind::QualityLevel => {
                if node.children.is_empty() {
                    return delay;
                }
                let i = self.quality.min(node.children.len() - 1);
                path.push(i);
                let e = self.expand(cue, &node.children[i], path, ctx, delay, play);
                path.pop();
                e
            }
            NodeKind::ParamCrossFade { param, inputs } => {
                let x = Self::param_float(play, param);
                let live = play.floats.iter().any(|(n, _)| n == param);
                let mut end = delay;
                for (i, c) in node.children.iter().enumerate() {
                    let g = inputs.get(i).map(|r| r.gain(x)).unwrap_or(1.0);
                    if g <= 0.0 && !live {
                        continue;
                    }
                    let mut cc = ctx.clone();
                    match (live, inputs.get(i)) {
                        (true, Some(r)) => cc.params.push((param.clone(), *r, x)),
                        _ => cc.volume *= g,
                    }
                    path.push(i);
                    end = end.max(self.expand(cue, c, path, &cc, delay, play));
                    path.pop();
                }
                end
            }
            NodeKind::DistanceCrossFade { inputs } => {
                let mut end = delay;
                for (i, c) in node.children.iter().enumerate() {
                    let mut cc = ctx.clone();
                    if let Some(r) = inputs.get(i) {
                        cc.fades.push(*r);
                    }
                    path.push(i);
                    end = end.max(self.expand(cue, c, path, &cc, delay, play));
                    path.pop();
                }
                end
            }
            NodeKind::Looping { count, indefinite } => {
                let mut end = delay;
                for (i, c) in node.children.iter().enumerate() {
                    path.push(i);
                    end = end.max(self.expand(cue, c, path, ctx, delay, play));
                    path.pop();
                }
                let iter_len = end.saturating_sub(delay);
                if iter_len == 0 || end >= FOREVER {
                    // nothing audible, or the child already loops forever on its own
                    return end;
                }
                let remaining = if *indefinite { None } else { Some(count.saturating_sub(1)) };
                if remaining != Some(0) {
                    self.scheduled.push(Scheduled {
                        at: self.clock + end,
                        cue: cue.clone(),
                        path: path.clone(),
                        ctx: ctx.clone(),
                        play: Arc::new(PlayInfoOwned { id: play.id, cue_key: play.cue_key, owner: play.owner, group: play.group, priority: play.priority, position: play.position, ints: play.ints.clone(), floats: play.floats.clone(), concurrency_scale: play.concurrency_scale }),
                        remaining,
                    });
                }
                if *indefinite {
                    FOREVER
                } else {
                    delay + iter_len * (*count as u64)
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn expand_all(&mut self, cue: &Arc<SoundCue>, node: &CueNode, path: &mut Vec<usize>, ctx: &Ctx, delay: u64, play: &PlayInfo, vols: Option<&Vec<f32>>) -> u64 {
        let mut end = delay;
        for (i, c) in node.children.iter().enumerate() {
            let mut cc;
            let cref = match vols {
                Some(v) => {
                    cc = ctx.clone();
                    cc.volume *= v.get(i).copied().unwrap_or(1.0);
                    &cc
                }
                None => ctx,
            };
            path.push(i);
            end = end.max(self.expand(cue, c, path, cref, delay, play));
            path.pop();
        }
        end
    }

    fn decode_lazy(a: &crate::soundwave::SoundWaveAsset, volume: f32, pitch: f32) -> Option<Arc<LoadedWave>> {
        let pcm = crate::codec::decode_asset(a).ok()?;
        Some(Arc::new(LoadedWave { pcm, volume, pitch, stream: None, pcm16: Vec::new(), lazy: None }.compact()))
    }
    fn keep_decoded(&mut self, key: usize, lw: Arc<LoadedWave>) {
        self.decoded_bytes += lw.pcm16.len() * 2 + lw.pcm.samples.len() * 4;
        self.decoded.insert(key, (lw, self.clock));
        while self.decoded_bytes > DECODED_BUDGET && self.decoded.len() > 1 {
            let Some((&k, _)) = self.decoded.iter().filter(|(k, _)| **k != key).min_by_key(|(_, v)| v.1) else { break };
            if let Some((w, _)) = self.decoded.remove(&k) { self.decoded_bytes = self.decoded_bytes.saturating_sub(w.pcm16.len() * 2 + w.pcm.samples.len() * 4); }
        }
    }
    fn collect_decoded(&mut self) {
        let done = std::mem::take(&mut *self.decode_done.lock().unwrap_or_else(|p| p.into_inner()));
        for (key, r) in done {
            self.decoding.remove(&key);
            match r {
                Some(lw) => {
                    for v in self.voices.iter_mut().filter(|v| v.pending == Some(key)) { v.wave = lw.clone(); v.pending = None; }
                    self.keep_decoded(key, lw);
                }
                None => { for v in self.voices.iter_mut().filter(|v| v.pending == Some(key)) { v.done = true; v.pending = None; } }
            }
        }
    }
    fn decoded_wave(&mut self, wave: &Arc<LoadedWave>) -> std::result::Result<Arc<LoadedWave>, Option<usize>> {
        let Some(a) = wave.lazy.clone() else { return Ok(wave.clone()) };
        let key = Arc::as_ptr(&a) as usize;
        if let Some(e) = self.decoded.get_mut(&key) {
            e.1 = self.clock;
            return Ok(e.0.clone());
        }
        if self.wait_streams {
            if let Some(lw) = Self::decode_lazy(&a, wave.volume, wave.pitch) { self.keep_decoded(key, lw.clone()); return Ok(lw); }
            return Err(None);
        }
        if self.decoding.insert(key) {
            if self.decode_tx.is_none() {
                let (tx, rx) = std::sync::mpsc::channel::<(usize, Arc<crate::soundwave::SoundWaveAsset>, f32, f32)>();
                let out = self.decode_done.clone();
                if std::thread::Builder::new().name("fn-audio-decode".into()).spawn(move || {
                    while let Ok((k, a, v, p)) = rx.recv() { let r = Self::decode_lazy(&a, v, p); out.lock().unwrap_or_else(|p| p.into_inner()).push((k, r)); }
                }).is_ok() { self.decode_tx = Some(tx); }
            }
            let sent = self.decode_tx.as_ref().is_some_and(|tx| tx.send((key, a.clone(), wave.volume, wave.pitch)).is_ok());
            if !sent {
                self.decoding.remove(&key);
                if let Some(lw) = Self::decode_lazy(&a, wave.volume, wave.pitch) { self.keep_decoded(key, lw.clone()); return Ok(lw); }
                return Err(None);
            }
        }
        Err(Some(key))
    }

    fn spawn_voice(&mut self, wave: Arc<LoadedWave>, looping: bool, ctx: &Ctx, delay: u64, play: &PlayInfo) -> u64 {
        let (wave, pending) = match self.decoded_wave(&wave) { Ok(w) => (w, None), Err(Some(k)) => (wave, Some(k)), Err(None) => return delay };
        if pending.is_some() && wave.lazy.as_ref().map_or(true, |a| a.info.sample_rate.is_none()) { return delay; }
        if self.voices.iter().filter(|v| !v.done).count() >= self.max_voices {
            // steal: lowest priority, then oldest; never a voice with higher priority than the newcomer
            let victim = self
                .voices
                .iter()
                .enumerate()
                .filter(|(_, v)| !v.done && v.priority <= play.priority + f32::EPSILON)
                .min_by(|a, b| a.1.priority.partial_cmp(&b.1.priority).unwrap_or(std::cmp::Ordering::Equal).then(b.1.age.cmp(&a.1.age)))
                .map(|(i, _)| i);
            match victim {
                Some(i) => {
                    self.voices[i].done = true;
                    self.stats.voices_stolen += 1;
                }
                None => return delay,
            }
        }
        let stream = match &wave.stream {
            Some(s) => match s.open(0, looping) {
                Some(c) => Some(c),
                None => {
                    self.stats.stream_starved += 1;
                    return delay;
                }
            },
            None => None,
        };
        let pitch = ctx.pitch * wave.pitch * ctx.class.pitch;
        let rate = wave.pcm.sample_rate as f64 / self.out_rate as f64 * pitch.max(0.01) as f64;
        let frames = match (&pending, &wave.lazy) { (Some(_), Some(a)) => a.info.duration.unwrap_or(0.) * wave.pcm.sample_rate as f64, _ => wave.frames() as f64 };
        let end = if looping { FOREVER } else { delay + (frames / rate) as u64 };
        let base_gain = ctx.volume * wave.volume * ctx.class.base_volume * play.concurrency_scale;
        let spatial = ctx.atten.is_some() && play.position.is_some();
        self.stats.voices_started += 1;
        self.voices.push(Voice {
            play: play.id,
            cue_key: play.cue_key,
            group: play.group,
            owner: play.owner,
            priority: play.priority,
            wave,
            pos: 0.0,
            delay,
            rate,
            looping,
            stream,
            base_gain,
            class: ctx.class.clone(),
            atten: if spatial { ctx.atten.clone() } else { None },
            position: if spatial { play.position } else { None },
            fades: ctx.fades.clone(),
            env: ctx.env.clone(),
            params: ctx.params.clone(),
            gl: 0.0,
            gr: 0.0,
            lpf: [0.0; 2],
            age: 0,
            fade_out: None,
            started: false,
            done: false,
            last_gain: 0.0,
            pending,
        });
        end
    }

    // ------------------------------------------------------------------ rendering

    fn mix_gain(&self, class: &ClassInfo) -> f32 {
        let mut g = 1.0;
        for m in &self.mixes {
            let mix = &m.def.mix;
            let t = m.age - mix.initial_delay;
            if t < 0.0 {
                continue;
            }
            // fade factor 0..1
            let fin = if mix.fade_in_time > 0.0 { (t / mix.fade_in_time).clamp(0.0, 1.0) } else { 1.0 };
            let pop = match (m.popped_at, mix.duration) {
                (Some(p), _) => Some(p),
                (None, d) if d >= 0.0 => Some(mix.initial_delay + mix.fade_in_time + d),
                _ => None,
            };
            let fout = match pop {
                Some(p) => {
                    let since = m.age - p.max(mix.initial_delay);
                    if since <= 0.0 {
                        1.0
                    } else if mix.fade_out_time > 0.0 {
                        (1.0 - since / mix.fade_out_time).clamp(0.0, 1.0)
                    } else {
                        0.0
                    }
                }
                None => 1.0,
            };
            let f = fin * fout;
            for a in &mix.adjusters {
                let ac = a.class.to_ascii_lowercase();
                let applies = class.chain.first().map_or(false, |c| *c == ac) || (a.apply_to_children && class.chain.iter().any(|c| *c == ac));
                if applies {
                    g *= 1.0 + (a.volume - 1.0) * f;
                }
            }
        }
        g
    }

    fn bus_gain(&self, bus: Bus) -> f32 {
        let master = self.bus[Bus::Master.index()];
        if bus == Bus::Master {
            master
        } else {
            master * self.bus[bus.index()]
        }
    }

    fn voice_targets(&self, v: &Voice, dt: f32) -> (f32, f32, Option<f32>, f32) {
        let mut gain = v.base_gain * self.bus_gain(v.class.bus) * self.mix_gain(&v.class);
        if let Some(c) = self.ctl.iter().find(|c| c.id == v.play) {
            gain *= c.cur; // MUSIC HOOK
        }
        let mut pitch_mul = 1.0;
        if let Some(e) = &v.env {
            let (ev, ep) = e.at(v.age as f32 / self.out_rate as f32);
            gain *= ev;
            pitch_mul = ep;
        }
        let mut pan_l = 1.0f32;
        let mut pan_r = 1.0f32;
        let mut blend = 0.0f32;
        let mut cutoff = None;
        // AMBIENCE HOOK: live adjustments of this play (gain ramp, position, attenuation override, explicit attenuation distance, extra low-pass)
        let adj = self.adj.iter().find(|c| c.id == v.play);
        let (vpos, vatt) = match adj {
            Some(a) => (a.position.or(v.position), a.atten.clone().or_else(|| v.atten.clone())),
            None => (v.position, v.atten.clone()),
        };
        if let Some(a) = adj {
            gain *= a.cur;
        }
        for (n, r, x0) in &v.params {
            let x = adj.and_then(|a| a.floats.iter().find(|f| f.0 == *n)).map_or(*x0, |f| f.1);
            gain *= r.gain(x);
        }
        if let (Some(p), Some(a)) = (vpos, &vatt) {
            let rel = p.sub(self.listener.position);
            let dist_pan = rel.length();
            let dist = adj.and_then(|a| a.dist).unwrap_or(dist_pan);
            gain *= a.gain(dist);
            for f in &v.fades {
                gain *= f.gain(dist);
            }
            cutoff = a.lpf_cutoff(dist);
            blend = a.spatial_blend(dist);
            if blend > 0.0 && dist_pan > 1e-3 {
                let dir = Vec3::new(rel.x / dist_pan, rel.y / dist_pan, rel.z / dist_pan);
                let fwd = self.listener.forward.normalized();
                let right = fwd.cross(self.listener.up).normalized();
                let x = dir.dot(right);
                let y = dir.dot(fwd);
                // azimuth folded to the front half-plane so a source behind pans like its mirror image in front
                let mut theta = x.atan2(y);
                if y < 0.0 {
                    theta = x.signum() * std::f32::consts::PI - theta;
                }
                let pan = theta.sin() * blend;
                let ang = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
                pan_l = ang.cos() * std::f32::consts::SQRT_2;
                pan_r = ang.sin() * std::f32::consts::SQRT_2;
            }
        } else {
            for f in &v.fades {
                gain *= f.gain(0.0);
            }
        }
        if let Some(l) = adj.and_then(|a| a.lpf_hz) {
            cutoff = Some(cutoff.map_or(l, |c: f32| c.min(l))); // AMBIENCE HOOK: SetLowPassFilterFrequency
        }
        if let Some(n) = v.fade_out {
            gain *= (n as f32 / 256.0).clamp(0.0, 1.0);
        }
        let _ = dt;
        // blend between untouched source channels (1,1) and the panned image
        let gl = gain * (1.0 - blend + blend * pan_l);
        let gr = gain * (1.0 - blend + blend * pan_r);
        (gl, gr, cutoff, pitch_mul)
    }

    /// Render `out.len() / 2` stereo frames (interleaved L,R), adding to nothing: `out` is overwritten.
    pub fn render(&mut self, out: &mut [f32]) {
        for s in out.iter_mut() {
            *s = 0.0;
        }
        let frames = out.len() / 2;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(BLOCK);
            self.drain_commands();
            self.collect_decoded();
            self.fire_scheduled(n as u64);
            self.render_block(&mut out[done * 2..(done + n) * 2], n);
            done += n;
        }
    }

    fn fire_scheduled(&mut self, block: u64) {
        let now = self.clock;
        let mut i = 0;
        while i < self.scheduled.len() {
            if self.scheduled[i].at < now + block {
                let s = self.scheduled.swap_remove(i);
                let delay = s.at.saturating_sub(now);
                let Some(root) = &s.cue.root else { continue };
                let Some(node) = Self::node_at(root, &s.path) else { continue };
                let NodeKind::Looping { .. } = &node.kind else { continue };
                let info = s.play.as_info();
                let mut path = s.path.clone();
                let mut end = delay;
                let cue = s.cue.clone();
                for (ci, c) in node.children.iter().enumerate() {
                    path.push(ci);
                    end = end.max(self.expand(&cue, c, &mut path, &s.ctx, delay, &info));
                    path.pop();
                }
                let iter_len = end.saturating_sub(delay);
                let remaining = s.remaining.map(|r| r.saturating_sub(1));
                if iter_len > 0 && end < FOREVER && remaining != Some(0) {
                    self.scheduled.push(Scheduled { at: now + end, cue: s.cue, path: s.path, ctx: s.ctx, play: s.play, remaining });
                }
                // `swap_remove` moved an unvisited element into slot i; push may append: keep i
            } else {
                i += 1;
            }
        }
    }

    fn render_block(&mut self, out: &mut [f32], n: usize) {
        let dt = n as f32 / self.out_rate as f32;
        // suspend ramp
        let target_susp = if self.suspended { 0.0 } else { 1.0 };
        let susp_from = self.suspend_gain;
        let susp_step = (target_susp - susp_from) / n as f32;
        self.suspend_gain = target_susp;
        let sr = self.out_rate as f32;
        let mut voices = std::mem::take(&mut self.voices);
        for v in &mut voices {
            if v.done || v.pending.is_some() {
                continue;
            }
            let (tl, tr, cutoff, pitch_mul) = self.voice_targets(v, dt);
            if !v.started {
                // first block: start at the target gain (no fade-in ramp from silence)
                v.gl = tl;
                v.gr = tr;
                v.started = true;
            }
            let (gl0, gr0) = (v.gl, v.gr);
            let (dgl, dgr) = ((tl - gl0) / n as f32, (tr - gr0) / n as f32);
            v.last_gain = tl.max(tr);
            let frames = v.wave.frames();
            let ch = v.wave.pcm.channels as usize;
            let src = &v.wave.pcm.samples;
            let src16 = &v.wave.pcm16;
            let rate = v.rate * pitch_mul as f64;
            let lpf_a = cutoff.map(|f| 1.0 - (-2.0 * std::f32::consts::PI * f / sr).exp());
            // a streamed wave loops over its whole length (the SoundWave's bLooping loop points are 0..frames)
            let streaming = v.stream.is_some();
            let loop_end = if streaming { frames } else { v.wave.pcm.loop_end.map(|e| (e as usize).min(frames)).unwrap_or(frames) };
            let loop_start = if streaming { 0 } else { v.wave.pcm.loop_start.map(|s| s as usize).unwrap_or(0).min(loop_end.saturating_sub(1)) };
            let mut st_end = u64::MAX;
            if let Some(c) = v.stream.as_mut() {
                if self.wait_streams {
                    // offline render: wait for the worker so the output never depends on its timing
                    let need = c.base + (v.pos + rate * n as f64) as u64 + 2;
                    c.wait_for(need, std::time::Duration::from_secs(10));
                }
                c.refresh();
                st_end = c.end();
            }
            for f in 0..n {
                if v.delay > 0 {
                    v.delay -= 1;
                    continue;
                }
                let i0 = v.pos as usize;
                let (l0, l1, r0, r1);
                if let Some(c) = v.stream.as_mut() {
                    let s0 = c.base + i0 as u64;
                    if i0 >= frames || frames == 0 || ch == 0 || (!v.looping && s0 >= st_end) {
                        v.done = true;
                        break;
                    }
                    let mut s1 = s0 + 1;
                    if !v.looping && (i0 + 1 >= loop_end || s1 >= st_end) {
                        s1 = s0;
                    }
                    if s1 >= c.available() {
                        c.note_stall(1);
                        continue; // underrun: stall (silence for this frame), never read stale data
                    }
                    (l0, r0) = c.frame(s0);
                    (l1, r1) = c.frame(s1);
                } else {
                    if i0 >= frames || frames == 0 || ch == 0 {
                        if v.looping && frames > 0 {
                            v.pos = loop_start as f64 + (v.pos - loop_end as f64).max(0.0);
                            continue;
                        }
                        v.done = true;
                        break;
                    }
                    let mut i1 = i0 + 1;
                    if i1 >= loop_end {
                        i1 = if v.looping { loop_start } else { i0 };
                    }
                    let at = |i: usize, c: usize| if src16.is_empty() { src[i * ch + c.min(ch - 1)] } else { src16[i * ch + c.min(ch - 1)] as f32 * (1.0 / 32768.0) };
                    (l0, l1) = (at(i0, 0), at(i1, 0));
                    (r0, r1) = if ch > 1 { (at(i0, 1), at(i1, 1)) } else { (l0, l1) };
                }
                let frac = (v.pos - i0 as f64) as f32;
                let mut sl = l0 + (l1 - l0) * frac;
                let mut sr_ = r0 + (r1 - r0) * frac;
                if let Some(a) = lpf_a {
                    v.lpf[0] += a * (sl - v.lpf[0]);
                    v.lpf[1] += a * (sr_ - v.lpf[1]);
                    sl = v.lpf[0];
                    sr_ = v.lpf[1];
                } else {
                    v.lpf = [sl, sr_];
                }
                let gl = gl0 + dgl * f as f32;
                let gr = gr0 + dgr * f as f32;
                let s = susp_from + susp_step * f as f32;
                out[f * 2] += sl * gl * s;
                out[f * 2 + 1] += sr_ * gr * s;
                v.pos += rate;
                if v.looping && v.pos >= loop_end as f64 {
                    v.pos = loop_start as f64 + (v.pos - loop_end as f64);
                    if let Some(c) = v.stream.as_mut() {
                        c.base += (loop_end - loop_start) as u64; // the ring continues with the next pass
                    }
                }
            }
            if let Some(c) = v.stream.as_ref() {
                c.consumed(c.base + v.pos as u64);
            }
            v.gl = tl;
            v.gr = tr;
            v.age += n as u64;
            if let Some(left) = v.fade_out {
                let nl = left.saturating_sub(n as u32);
                v.fade_out = Some(nl);
                if nl == 0 {
                    v.done = true;
                }
            }
        }
        voices.retain(|v| !v.done);
        // voices spawned during rendering (none today) would be in self.voices; keep order stable
        voices.append(&mut self.voices);
        self.voices = voices;
        if !self.ctl.is_empty() {
            // MUSIC HOOK: advance the per-play gain ramps; a play faded to silence is stopped; ramps of plays that no longer exist are dropped
            let mut stops = Vec::new();
            for c in &mut self.ctl {
                let step = c.rate * dt;
                c.cur = if (c.target - c.cur).abs() <= step { c.target } else { c.cur + step.copysign(c.target - c.cur) };
                if c.stop && c.cur <= 0.0 {
                    stops.push(c.id);
                }
            }
            for id in stops {
                self.stop_play(id);
                self.ctl.retain(|c| c.id != id);
            }
            let (voices, sched) = (&self.voices, &self.scheduled);
            self.ctl.retain(|c| voices.iter().any(|v| v.play == c.id) || sched.iter().any(|s| s.play.id == c.id));
        }
        if !self.adj.is_empty() {
            // AMBIENCE HOOK: advance the per-play ramps (`playadj`); a play faded to silence with the stop flag ends; entries of finished plays are dropped
            for id in crate::playadj::advance(&mut self.adj, dt) {
                self.stop_play(id);
                self.adj.retain(|c| c.id != id);
            }
            let (voices, sched) = (&self.voices, &self.scheduled);
            self.adj.retain(|c| voices.iter().any(|v| v.play == c.id) || sched.iter().any(|s| s.play.id == c.id));
        }
        for m in &mut self.mixes {
            m.age += dt;
        }
        self.mixes.retain(|m| {
            let mix = &m.def.mix;
            let end = match m.popped_at {
                Some(p) => Some(p + mix.fade_out_time),
                None if mix.duration >= 0.0 => Some(mix.initial_delay + mix.fade_in_time + mix.duration + mix.fade_out_time),
                None => None,
            };
            end.map_or(true, |e| m.age < e + 0.01)
        });
        self.clock += n as u64;
    }
}

/// Owner of a connected handle + mixer for offline (deterministic) rendering and as the source of the pair for the
/// device stream.
pub struct AudioEngine {
    handle: EngineHandle,
    mixer: Mixer,
}

impl AudioEngine {
    pub fn new(sample_rate: u32) -> AudioEngine {
        let (handle, mixer) = engine_pair(sample_rate);
        AudioEngine { handle, mixer }
    }
    pub fn handle(&self) -> EngineHandle {
        self.handle.clone()
    }
    pub fn mixer(&self) -> &Mixer {
        &self.mixer
    }
    pub fn mixer_mut(&mut self) -> &mut Mixer {
        &mut self.mixer
    }
    pub fn into_parts(self) -> (EngineHandle, Mixer) {
        (self.handle, self.mixer)
    }
    /// Render `frames` stereo frames. The RNG is re-seeded first so identical command sequences give identical audio.
    pub fn render_offline(&mut self, frames: usize, seed: u64) -> Vec<f32> {
        self.mixer.set_seed(seed);
        let mut out = vec![0.0; frames * 2];
        self.mixer.wait_streams = true; // streamed waves: never let worker timing change the output
        self.mixer.render(&mut out);
        self.mixer.wait_streams = false;
        out
    }
}






#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::Concurrency;
    use std::sync::Arc;

    fn wave(v: f32, frames: usize, rate: u32) -> LoadedWave {
        LoadedWave::new(DecodedWave::new(vec![v; frames], rate, 1))
    }
    fn node(kind: NodeKind, children: Vec<CueNode>) -> CueNode {
        CueNode { name: "n".into(), kind, children }
    }
    fn player(path: &str) -> CueNode {
        node(NodeKind::WavePlayer { wave: Some(path.into()), looping: false }, vec![])
    }
    fn cue(root: CueNode) -> SoundCue {
        SoundCue { name: "c".into(), path: "/Game/c".into(), root: Some(root), volume_multiplier: 1.0, pitch_multiplier: 1.0, priority: 1.0, duration: 0.0, class_name: None, class: ClassInfo::default(), attenuation: None, concurrency: None }
    }
    fn engine() -> AudioEngine {
        AudioEngine::new(48000)
    }
    fn add(e: &AudioEngine, name: &str, w: LoadedWave) {
        e.handle().add_wave(name, w).unwrap();
    }
    fn play(e: &AudioEngine, name: &str) {
        e.handle().play_cue(name, PlayParams::default()).unwrap();
    }

    #[test]
    fn compact_pcm16_wave_plays_like_the_f32_wave() {
        let w = wave(0.5, 1000, 48000).compact();
        assert!(w.pcm.samples.is_empty());
        assert_eq!(w.frames(), 1000);
        let mut e = engine();
        add(&e, "/Game/a", w);
        e.handle().add_cue("/Game/c", Arc::new(cue(player("/Game/a")))).unwrap();
        play(&e, "/Game/c");
        assert!((e.render_offline(8, 1)[0] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn class_volume_math_and_bus_sliders() {
        let mut e = engine();
        add(&e, "/Game/a", wave(1.0, 1000, 48000));
        let mut c = cue(player("/Game/a"));
        c.class.base_volume = 0.5;
        e.handle().add_cue("/Game/c", Arc::new(c)).unwrap();
        play(&e, "/Game/c");
        assert!((e.render_offline(8, 1)[0] - 0.5).abs() < 1e-5);
        e.handle().set_bus_volume(Bus::Master, 0.5);
        e.handle().set_bus_volume(Bus::Sfx, 0.5);
        let out = e.render_offline(256, 1);
        assert!((out[2 * 255] - 0.125).abs() < 1e-3, "{}", out[2 * 255]); // 0.5 x 0.5 x 0.5 after the ramp
    }

    #[test]
    fn random_is_deterministic_per_seed_and_weighted() {
        let mut e = engine();
        add(&e, "/Game/a", wave(0.25, 100, 48000));
        add(&e, "/Game/b", wave(0.75, 100, 48000));
        let r = node(NodeKind::Random { weights: vec![1.0, 0.0], without_replacement: false }, vec![player("/Game/a"), player("/Game/b")]);
        e.handle().add_cue("/Game/c", Arc::new(cue(r))).unwrap();
        for seed in 0..20 {
            play(&e, "/Game/c");
            let o = e.render_offline(64, seed);
            assert!((o[0] - 0.25).abs() < 1e-5, "weight 0 child must never play");
            e.handle().stop_all();
            e.render_offline(512, seed);
        }
        let r2 = node(NodeKind::Random { weights: vec![1.0, 1.0], without_replacement: false }, vec![player("/Game/a"), player("/Game/b")]);
        e.handle().add_cue("/Game/d", Arc::new(cue(r2))).unwrap();
        let mut seen = std::collections::HashSet::new();
        for seed in 0..40 {
            play(&e, "/Game/d");
            let o = e.render_offline(64, seed);
            seen.insert((o[0] * 100.0) as i32);
            e.handle().stop_all();
            e.render_offline(512, seed);
        }
        assert_eq!(seen.len(), 2, "both children get picked");
        play(&e, "/Game/d");
        let a = e.render_offline(64, 5);
        e.handle().stop_all();
        e.render_offline(512, 5);
        play(&e, "/Game/d");
        let b = e.render_offline(64, 5);
        assert_eq!(a, b);
    }

    #[test]
    fn without_replacement_cycles_through_all_children() {
        let mut e = engine();
        for (i, n) in ["a", "b", "c"].iter().enumerate() {
            add(&e, &format!("/Game/{n}"), wave(0.1 * (i + 1) as f32, 40, 48000));
        }
        let r = node(NodeKind::Random { weights: vec![], without_replacement: true }, vec![player("/Game/a"), player("/Game/b"), player("/Game/c")]);
        e.handle().add_cue("/Game/c1", Arc::new(cue(r))).unwrap();
        let mut seen = Vec::new();
        for _ in 0..3 {
            play(&e, "/Game/c1");
            let o = e.render_offline(64, 11);
            seen.push((o[0] * 10.0).round() as i32);
            e.render_offline(256, 11);
        }
        seen.sort();
        assert_eq!(seen, vec![1, 2, 3]);
    }

    #[test]
    fn mixer_modulator_delay_and_concatenator_timing() {
        let mut e = engine();
        add(&e, "/Game/a", wave(1.0, 480, 48000)); // 10 ms
        let m = node(NodeKind::Modulator { pitch: (1.0, 1.0), volume: (0.5, 0.5) }, vec![player("/Game/a")]);
        e.handle().add_cue("/Game/m", Arc::new(cue(m))).unwrap();
        play(&e, "/Game/m");
        assert!((e.render_offline(8, 1)[0] - 0.5).abs() < 1e-5);
        e.render_offline(2000, 1);
        // delay 10 ms
        let d = node(NodeKind::Delay { min: 0.01, max: 0.01 }, vec![player("/Game/a")]);
        e.handle().add_cue("/Game/d", Arc::new(cue(d))).unwrap();
        play(&e, "/Game/d");
        let o = e.render_offline(1200, 1);
        assert_eq!(o[2 * 400], 0.0);
        assert!(o[2 * 500] > 0.9);
        e.render_offline(2000, 1);
        // concatenator: two 10 ms clips back to back; the second has half volume
        let c = node(NodeKind::Concatenator { input_volume: vec![1.0, 0.5] }, vec![player("/Game/a"), player("/Game/a")]);
        e.handle().add_cue("/Game/cc", Arc::new(cue(c))).unwrap();
        play(&e, "/Game/cc");
        let o = e.render_offline(1500, 1);
        assert!((o[2 * 100] - 1.0).abs() < 1e-5 && (o[2 * 700] - 0.5).abs() < 1e-5 && o[2 * 1100] == 0.0);
    }

    #[test]
    fn looping_node_repeats_until_stopped() {
        let mut e = engine();
        add(&e, "/Game/a", wave(1.0, 480, 48000));
        let l = node(NodeKind::Looping { count: 0, indefinite: true }, vec![player("/Game/a")]);
        e.handle().add_cue("/Game/l", Arc::new(cue(l))).unwrap();
        play(&e, "/Game/l");
        let o = e.render_offline(48000, 1); // 1 s = ~100 iterations of 10 ms
        let silent = o.chunks(2).filter(|f| f[0] == 0.0).count();
        assert!(silent < 400, "loop must be near-gapless ({silent} silent frames)");
        let id = e.handle().play_cue("/Game/l", PlayParams::default()).unwrap();
        e.handle().stop_all();
        e.render_offline(1024, 1);
        let o = e.render_offline(2000, 1);
        assert!(o.iter().all(|&s| s == 0.0), "stop_all ends loops and cancels the schedule");
        let _ = id;
        // finite count
        let l = node(NodeKind::Looping { count: 3, indefinite: false }, vec![player("/Game/a")]);
        e.handle().add_cue("/Game/l3", Arc::new(cue(l))).unwrap();
        play(&e, "/Game/l3");
        let o = e.render_offline(4800, 1);
        let loud = o.chunks(2).filter(|f| f[0] > 0.5).count();
        assert!((1300..=1600).contains(&loud), "3 x 480 frames, got {loud}");
    }

    #[test]
    fn looping_wave_player_wraps_with_loop_points() {
        let mut e = engine();
        let mut w = wave(0.0, 100, 48000);
        w.pcm.samples = (0..100).map(|i| i as f32 / 100.0).collect();
        w.pcm.loop_start = Some(0);
        w.pcm.loop_end = Some(100);
        add(&e, "/Game/a", w);
        let n = node(NodeKind::WavePlayer { wave: Some("/Game/a".into()), looping: true }, vec![]);
        e.handle().add_cue("/Game/l", Arc::new(cue(n))).unwrap();
        play(&e, "/Game/l");
        let o = e.render_offline(1000, 1);
        // sample 250 is index 50 of the wave
        assert!((o[2 * 250] - 0.5).abs() < 0.02, "{}", o[2 * 250]);
        assert!(e.mixer().voice_count() == 1);
    }

    #[test]
    fn attenuation_pan_and_cull() {
        let mut e = engine();
        add(&e, "/Game/a", wave(1.0, 48000, 48000));
        let att = Arc::new(Attenuation { extents: [100.0, 0.0, 0.0], falloff_distance: 900.0, ..Default::default() });
        let mut c = cue(player("/Game/a"));
        c.attenuation = Some(att);
        e.handle().add_cue("/Game/c", Arc::new(c)).unwrap();
        let lvl = |e: &mut AudioEngine, pos: Vec3| {
            e.handle().set_listener(Vec3::default(), Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 1.0, 0.0));
            e.handle().play_cue("/Game/c", PlayParams { position: Some(pos), ..Default::default() }).unwrap();
            let o = e.render_offline(512, 1);
            e.handle().stop_all();
            e.render_offline(1024, 1);
            (o[2 * 500], o[2 * 500 + 1])
        };
        let (l, r) = lvl(&mut e, Vec3::new(500.0, 0.0, 0.0));
        assert!(r > l * 3.0, "source on the right pans right ({l},{r})");
        let (l, r) = lvl(&mut e, Vec3::new(-500.0, 0.0, 0.0));
        assert!(l > r * 3.0);
        let (l, r) = lvl(&mut e, Vec3::new(0.0, 0.0, -500.0));
        assert!((l - r).abs() < 1e-3, "front is centred");
        let (l, r) = lvl(&mut e, Vec3::new(0.0, 0.0, 500.0));
        assert!((l - r).abs() < 1e-3, "behind is centred too");
        let near = lvl(&mut e, Vec3::new(0.0, 0.0, -100.0));
        let far = lvl(&mut e, Vec3::new(0.0, 0.0, -600.0));
        assert!(near.0 > far.0 * 1.5);
        assert_eq!(lvl(&mut e, Vec3::new(0.0, 0.0, -1500.0)), (0.0, 0.0), "beyond inner + falloff = 1000 cm");
        // 2D play ignores the listener
        play(&e, "/Game/c");
        let o = e.render_offline(256, 1);
        assert!((o[2 * 200] - 1.0).abs() < 1e-3 && (o[2 * 200 + 1] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn concurrency_rules_and_voice_limit() {
        let mut e = engine();
        add(&e, "/Game/a", wave(0.1, 48000, 48000));
        let mut c = cue(player("/Game/a"));
        c.concurrency = Some(Concurrency { group: "g".into(), max_count: 2, rule: ResolutionRule::StopOldest, limit_to_owner: false, volume_scale: 1.0 });
        e.handle().add_cue("/Game/c", Arc::new(c)).unwrap();
        for _ in 0..5 {
            play(&e, "/Game/c");
            e.render_offline(64, 1);
        }
        e.render_offline(2048, 1); // let the stopped ones fade out
        assert_eq!(e.mixer().voice_count(), 2);
        assert!(e.mixer().stats.voices_stolen >= 3);
        let mut c2 = cue(player("/Game/a"));
        c2.concurrency = Some(Concurrency { group: "p".into(), max_count: 1, rule: ResolutionRule::PreventNew, limit_to_owner: false, volume_scale: 1.0 });
        e.handle().add_cue("/Game/p", Arc::new(c2)).unwrap();
        play(&e, "/Game/p");
        play(&e, "/Game/p");
        e.render_offline(64, 1);
        assert_eq!(e.mixer().stats.plays_prevented, 1);
        // global voice limit
        e.handle().stop_all();
        e.render_offline(2048, 1);
        e.handle().set_max_voices(4);
        e.handle().add_cue("/Game/plain", Arc::new(cue(player("/Game/a")))).unwrap();
        for _ in 0..10 {
            play(&e, "/Game/plain");
        }
        e.render_offline(64, 1);
        assert!(e.mixer().voice_count() <= 4);
    }

    #[test]
    fn suspend_mutes_and_restores() {
        let mut e = engine();
        add(&e, "/Game/a", wave(1.0, 48000, 48000));
        e.handle().add_cue("/Game/c", Arc::new(cue(player("/Game/a")))).unwrap();
        play(&e, "/Game/c");
        e.render_offline(256, 1);
        e.handle().set_suspended(true);
        e.render_offline(256, 1);
        let o = e.render_offline(256, 1);
        assert!(o.iter().all(|&s| s == 0.0));
        e.handle().set_suspended(false);
        e.render_offline(256, 1);
        let o = e.render_offline(64, 1);
        assert!((o[0] - 1.0).abs() < 1e-3);
    }

    /// MUSIC HOOK: `fade_play` ramps one play linearly over the given time and stops it at silence; other plays are untouched.
    #[test]
    fn fade_play_ramps_linearly_and_stops_at_silence() {
        let mut e = engine();
        add(&e, "/Game/a", wave(1.0, 48000 * 10, 48000));
        e.handle().add_cue("/Game/c", Arc::new(cue(player("/Game/a")))).unwrap();
        let h = e.handle();
        let a = h.play_cue("/Game/c", PlayParams::default()).unwrap();
        let b = h.play_cue("/Game/c", PlayParams::default()).unwrap();
        let o = e.render_offline(256, 1);
        assert!((o[2 * 255] - 2.0).abs() < 1e-3, "two plays of a 1.0 wave: {}", o[2 * 255]);
        h.fade_play(a, 0.0, 1.0, true);
        let o = e.render_offline(48000 / 2, 1); // half way through the 1 s fade: a ~0.5 -> sum ~1.5
        let mid = o[2 * (48000 / 2 - 1)];
        assert!((mid - 1.5).abs() < 0.03, "mid fade {mid}");
        let o = e.render_offline(48000, 1); // fade over (+ the 256-frame stop ramp), only b remains
        assert!((o[2 * (48000 - 1)] - 1.0).abs() < 1e-3, "after the fade {}", o[2 * (48000 - 1)]);
        assert_eq!(e.mixer().voice_count(), 1, "the faded play is gone");
        h.fade_play(b, 0.25, 0.0, false); // immediate set, keeps playing
        e.render_offline(256, 1);
        let o = e.render_offline(256, 1);
        assert!((o[2 * 255] - 0.25).abs() < 1e-3, "{}", o[2 * 255]);
    }

    #[test]
    fn resampling_pitch_and_rate_conversion() {
        let mut e = engine();
        // 24 kHz wave plays for twice as many output frames at 48 kHz
        add(&e, "/Game/a", wave(1.0, 2400, 24000));
        e.handle().add_cue("/Game/c", Arc::new(cue(player("/Game/a")))).unwrap();
        play(&e, "/Game/c");
        let o = e.render_offline(6000, 1);
        let loud = o.chunks(2).filter(|f| f[0] > 0.5).count();
        assert!((4790..=4810).contains(&loud), "{loud}");
        // pitch 2 halves the duration
        e.handle().play_cue("/Game/c", PlayParams { pitch: 2.0, ..Default::default() }).unwrap();
        let o = e.render_offline(6000, 1);
        let loud = o.chunks(2).filter(|f| f[0] > 0.5).count();
        assert!((2390..=2410).contains(&loud), "{loud}");
    }

    #[test]
    fn sound_mix_ducks_a_class() {
        let mut e = engine();
        add(&e, "/Game/a", wave(1.0, 96000, 48000));
        let mut c = cue(player("/Game/a"));
        c.class.chain = vec!["fort_weaponmelee".into(), "sfx".into(), "master".into()];
        e.handle().add_cue("/Game/c", Arc::new(c)).unwrap();
        play(&e, "/Game/c");
        e.render_offline(64, 1);
        let mix = SoundMix { name: "duck".into(), initial_delay: 0.0, fade_in_time: 0.0, duration: -1.0, fade_out_time: 0.0, adjusters: vec![crate::soundclass::ClassAdjuster { class: "SFX".into(), volume: 0.25, pitch: 1.0, apply_to_children: true }] };
        e.handle().push_mix(mix);
        e.render_offline(512, 1);
        let o = e.render_offline(64, 1);
        assert!((o[0] - 0.25).abs() < 1e-3, "{}", o[0]);
        e.handle().pop_mix("duck");
        e.render_offline(512, 1);
        let o = e.render_offline(64, 1);
        assert!((o[0] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn switch_and_param_crossfade() {
        let mut e = engine();
        add(&e, "/Game/a", wave(0.2, 4800, 48000));
        add(&e, "/Game/b", wave(0.4, 4800, 48000));
        let s = node(NodeKind::Switch { param: "FS_Surfaces".into() }, vec![player("/Game/a"), player("/Game/b")]);
        e.handle().add_cue("/Game/s", Arc::new(cue(s))).unwrap();
        e.handle().play_cue("/Game/s", PlayParams { ints: vec![("FS_Surfaces".into(), 0)], ..Default::default() }).unwrap();
        assert!((e.render_offline(64, 1)[0] - 0.4).abs() < 1e-5);
        e.render_offline(8000, 1);
        let fr = |a: f32, b: f32| FadeRange { fade_in_start: a, fade_in_end: a, fade_out_start: b, fade_out_end: b + 0.0001, volume: 1.0 };
        let p = node(NodeKind::ParamCrossFade { param: "P".into(), inputs: vec![fr(-1.0, 0.5), fr(0.5, 2.0)] }, vec![player("/Game/a"), player("/Game/b")]);
        e.handle().add_cue("/Game/p", Arc::new(cue(p))).unwrap();
        e.handle().play_cue("/Game/p", PlayParams { floats: vec![("P".into(), 0.0)], ..Default::default() }).unwrap();
        assert!((e.render_offline(64, 1)[0] - 0.2).abs() < 1e-5, "only the first input is active at 0");
    }

    #[test]
    fn param_crossfade_follows_live_parameter_updates() {
        let mut e = engine();
        add(&e, "/Game/a", wave(0.2, 48000, 48000));
        add(&e, "/Game/b", wave(0.4, 48000, 48000));
        let fr = |a: f32, b: f32| FadeRange { fade_in_start: a, fade_in_end: a, fade_out_start: b, fade_out_end: b + 0.0001, volume: 1.0 };
        let p = node(NodeKind::ParamCrossFade { param: "P".into(), inputs: vec![fr(-1.0, 0.5), fr(0.5, 2.0)] }, vec![player("/Game/a"), player("/Game/b")]);
        e.handle().add_cue("/Game/p", Arc::new(cue(p))).unwrap();
        let id = e.handle().play_cue("/Game/p", PlayParams { floats: vec![("P".into(), 0.0)], ..Default::default() }).unwrap();
        assert!((e.render_offline(64, 1)[0] - 0.2).abs() < 1e-5);
        e.handle().adjust_play(id, crate::playadj::Adjust { floats: vec![("P".into(), 1.0)], ..Default::default() });
        e.render_offline(4800, 1);
        let o = e.render_offline(64, 1);
        assert!((o[0] - 0.4).abs() < 1e-3, "{}", o[0]);
    }

    #[test]
    fn unknown_cue_and_full_queue_are_not_fatal() {
        let mut e = engine();
        play(&e, "/Game/nothing");
        let o = e.render_offline(64, 1);
        assert!(o.iter().all(|&s| s == 0.0));
        assert_eq!(e.mixer().stats.unknown_cues, 1);
        let h = e.handle();
        let mut err = 0;
        for _ in 0..20_000 {
            if h.play_cue("/Game/x", PlayParams::default()).is_err() {
                err += 1;
            }
        }
        assert!(err > 0 && h.dropped_commands() > 0);
        e.render_offline(64, 1);
    }
}
