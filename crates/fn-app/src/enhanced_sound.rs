use fn_animgraph::enhanced_anim as ea;
use fn_audio::attenuation::{Attenuation, DistanceModel, Shape};
use fn_audio::cue::{CueNode, FadeRange, NodeKind, SoundCue};
use fn_audio::curve::{Curve, Key};
use fn_audio::mixer::LoadedWave;
use fn_audio::soundclass::{Bus, ClassInfo};
use fn_audio_sink::AudioSink;
use fn_game::GameSim;
use glam::Vec3;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

pub const DIR: &str = "sound-enhanced";

const SURFACE_NAMES: [&str; 14] = ["Dirt", "Grass", "Metal", "Sand", "Snow", "Ice", "Stone", "Water", "Wood", "Chrome", "ChromePetrified", "Mud", "WetConcrete", "TreeCanopy"];
const SURFACE_MAX_NONE: u8 = 14;
const PHYS_TO_FOOTSTEP: [(u32, u8); 14] = [(1, 8), (2, 6), (3, 2), (8, 7), (9, 1), (23, 4), (24, 5), (26, 3), (27, 6), (28, 9), (29, 10), (30, 11), (31, 12), (32, 13)];
const PHYS_WATER: u32 = 8;
const PHYS_REACHABLE: [u32; 5] = [0, 1, 2, 3, PHYS_WATER];
const STANCE_WALK: i32 = 0;
const STANCE_RUN: i32 = 1;
const STANCE_SPRINT: i32 = 2;
const STANCE_CROUCH_WALK: i32 = 3;
const STANCE_CROUCH_RUN: i32 = 4;
const RELATION_LOCAL: i32 = 0;
const ELEVATION_TILE_RANGE: f32 = 3.0;
const ELEVATION_TILE_CM: f32 = 384.0;
const PITCH_MAX_ABOVE: f32 = 1.2;
const PITCH_MAX_BELOW: f32 = 0.8;
const PITCH_DEADZONE_PCT: f32 = 0.95;
const SLIDE_LOOP_FADE_IN: f32 = 1.0;
const SLIDE_LOOP_FADE_OUT: f32 = 0.25;
const SLIDE_STOP_FADE_OUT: f32 = 0.15;
const SLIDE_PARAM_PERIOD: f32 = 0.1;
const SLIDE_VELOCITY_RANGE: (f32, f32) = (0., 1000.);
const SLIDE_ZVELOCITY_RANGE: (f32, f32) = (500., 1200.);
const SURFACE_TRACE_UP_M: f32 = 0.5;
const SURFACE_TRACE_LEN_M: f32 = 1.5;
const GC_SLIDING: &str = "GameplayCue.Athena.Player.Sliding.Player";
const GC_CLAMBER: &str = "GameplayCue.Athena.Player.Cucumber.Active";
const EV_STEP: &str = "SoundLibrary.Foley.Step";
const LOCAL_FLAGS: [(&str, i32); 3] = [("IsLocal", 1), ("IsTeam", 1), ("IsEnemy", 0)];

#[derive(Clone, Copy, PartialEq)]
enum Kind { Lib, State, Step, Gc, Local }

struct Row { t: f32, kind: Kind, ev: Option<String>, surf: Option<u8>, stance: Option<i32>, vol: f32, pitch: f32, chance: f32, thr: f32, cue: Option<String>, gc: Option<String> }

struct Burst { cue: String, delay: f32, chance: f32 }
struct LoopParam { name: String, attack: f32, release: f32 }
#[derive(Default)]
struct Gc { burst: Vec<Burst>, start: Option<String>, stop: Option<String>, loop_cue: Option<String>, params: Vec<LoopParam> }

struct Meta { libs: Vec<(String, HashMap<String, String>)>, notifies: HashMap<String, Vec<Row>>, gcs: HashMap<String, Gc>, hand: String }

struct Loaded { meta: Meta, waves: Vec<(String, Arc<LoadedWave>)>, cues: Vec<(String, Arc<SoundCue>)>, pcm_bytes: usize }

#[derive(Default)]
struct Slide { on: bool, loop_id: u64, stop_id: u64, next_param: f32, params: HashMap<String, f32> }

#[derive(Default)]
struct State {
    started: bool,
    rx: Option<Receiver<Result<Loaded, String>>>,
    pending: Option<(Vec<(String, Arc<LoadedWave>)>, Vec<(String, Arc<SoundCue>)>)>,
    meta: Option<Arc<Meta>>,
    last_steps: u64,
    prev: HashMap<(u8, String), f32>,
    montage: Option<&'static str>,
    slide: Slide,
    delayed: Vec<(f32, String)>,
    clock: f32,
    rng: u64,
    tracks: Vec<ea::NotifyTrack>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R { let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner()); f(g.get_or_insert_with(State::default)) }

pub fn ensure_loading(roots: &[PathBuf]) {
    if with(|s| s.started) { return; }
    with(|s| s.started = true);
    if fn_live::var("FN_ENH_SOUND").is_ok_and(|v| v == "0") { eprintln!("[enhanced-sound] off (FN_ENH_SOUND=0)"); return; }
    let Some(folder) = roots.iter().map(|r| r.join(DIR)).find(|d| d.join("index.json").is_file()) else { eprintln!("[enhanced-sound] no `{DIR}` folder in the asset roots: Enhanced mode has no 30.0 foley"); return };
    let (tx, rx) = std::sync::mpsc::channel();
    with(|s| s.rx = Some(rx));
    let _ = std::thread::Builder::new().name("enhanced-sound-load".into()).spawn(move || { let t = std::time::Instant::now(); let r = load(&folder); if let Ok(l) = &r { eprintln!("[enhanced-sound] {} cues, {} waves ({:.1} MB PCM) decoded in {:.0} ms", l.cues.len(), l.waves.len(), l.pcm_bytes as f64 / 1048576., t.elapsed().as_secs_f32() * 1000.); } let _ = tx.send(r); });
}

fn receive(st: &mut State) {
    let Some(rx) = &st.rx else { return };
    let Ok(r) = rx.try_recv() else { return };
    st.rx = None;
    match r {
        Ok(l) => { eprintln!("[enhanced-sound] ready ({}): libraries {:?}", l.meta.hand, l.meta.libs.iter().map(|(k, m)| format!("{k} ({} tags)", m.len())).collect::<Vec<_>>()); st.pending = Some((l.waves, l.cues)); st.meta = Some(Arc::new(l.meta)); st.rng = 0x9e37_79b9_7f4a_7c15; }
        Err(e) => eprintln!("[enhanced-sound] unavailable: {e}"),
    }
}

pub fn replaces_steps(sim: &GameSim) -> bool {
    let e = &sim.anim.enh;
    e.input.enabled && (e.tactical_weight() > 0.5 || e.slide_weight() > 0.5 || e.montage_weight() > 0.5) && with(|s| s.meta.is_some())
}

pub fn ready() -> bool { with(|s| { receive(s); s.rx.is_none() }) }

fn f32_of(v: &Value, d: f32) -> f32 { v.as_f64().map_or(d, |x| x as f32) }

fn enum_index(v: &Value, names: &[&[&str]]) -> Option<usize> {
    if let Some(n) = v.as_i64() { return Some(n as usize); }
    let s = v.as_str()?;
    let tail = s.rsplit("::").next().unwrap_or(s);
    if let Ok(n) = tail.parse::<usize>() { return Some(n); }
    names.iter().position(|alts| alts.iter().any(|a| a.eq_ignore_ascii_case(tail)))
}

fn attenuation(p: &Value) -> Attenuation {
    let mut a = Attenuation::default();
    let b = |k: &str| p.get(k).and_then(Value::as_bool);
    let f = |k: &str| p.get(k).and_then(Value::as_f64).map(|x| x as f32);
    if let Some(v) = b("bAttenuate") { a.attenuate = v; }
    if let Some(v) = b("bSpatialize") { a.spatialize = v; }
    if let Some(i) = p.get("AttenuationShape").and_then(|v| enum_index(v, &[&["Sphere", "AAM_None"], &["Capsule", "AAM_FXAA"], &["Box", "AAM_TemporalAA"], &["Cone", "AAM_MSAA"]])) {
        a.shape = [Shape::Sphere, Shape::Capsule, Shape::Box, Shape::Cone][i.min(3)];
    }
    if let Some(e) = p.get("AttenuationShapeExtents").and_then(Value::as_array) { for (i, x) in e.iter().take(3).enumerate() { a.extents[i] = f32_of(x, 0.); } }
    if let Some(v) = f("FalloffDistance") { a.falloff_distance = v; }
    if let Some(i) = p.get("DistanceAlgorithm").and_then(|v| enum_index(v, &[&["ATTENUATION_Linear", "Linear"], &["ATTENUATION_Logarithmic", "Logarithmic"], &["ATTENUATION_Inverse", "Inverse"], &["ATTENUATION_LogReverse", "LogReverse"], &["ATTENUATION_NaturalSound", "NaturalSound"], &["ATTENUATION_Custom", "Custom"]])) {
        a.algorithm = [DistanceModel::Linear, DistanceModel::Logarithmic, DistanceModel::Inverse, DistanceModel::LogReverse, DistanceModel::NaturalSound, DistanceModel::Custom][i.min(5)];
    }
    if let Some(v) = f("dBAttenuationAtMax") { a.db_attenuation_at_max = v; }
    if let Some(keys) = p.get("CustomAttenuationCurve").and_then(Value::as_array) {
        a.custom_curve = Curve(keys.iter().filter_map(|k| Some(Key { time: f32_of(k.get(0)?, 0.), value: f32_of(k.get(1)?, 0.), interp: 0, arrive_tangent: 0., leave_tangent: 0. })).collect());
    }
    if let Some(v) = f("OmniRadius") { a.omni_radius = v; }
    if let Some(v) = f("StereoSpread") { a.stereo_spread = v; }
    if let Some(v) = b("bAttenuateWithLPF") { a.lpf_enabled = v; }
    if let Some(v) = f("LPFRadiusMin") { a.lpf_radius_min = v; }
    if let Some(v) = f("LPFRadiusMax") { a.lpf_radius_max = v; }
    if let Some(v) = f("LPFFrequencyAtMin") { a.lpf_freq_at_min = v; }
    if let Some(v) = f("LPFFrequencyAtMax") { a.lpf_freq_at_max = v; }
    if let Some(v) = b("bEnableListenerFocus") { a.listener_focus = v; }
    if let Some(v) = f("FocusAzimuth") { a.focus_azimuth = v; }
    if let Some(v) = f("NonFocusAzimuth") { a.non_focus_azimuth = v; }
    if let Some(v) = f("FocusVolumeAttenuation") { a.focus_volume = v; }
    if let Some(v) = f("NonFocusVolumeAttenuation") { a.non_focus_volume = v; }
    a
}

fn class_info(classes: &Value, key: &str) -> ClassInfo {
    let mut chain = vec![];
    let mut cur = Some(key.to_owned());
    while let Some(k) = cur.take() {
        if chain.len() > 32 { break; }
        let Some(c) = classes.get(&k) else { break };
        chain.push((k.rsplit('/').next().unwrap_or(&k).to_owned(), c.clone()));
        cur = c.get("parent").and_then(Value::as_str).map(str::to_owned);
    }
    if chain.is_empty() { return ClassInfo::default(); }
    let base_volume = chain.iter().map(|(_, c)| f32_of(&c["volume"], 1.)).product::<f32>();
    let pitch = chain.iter().map(|(_, c)| f32_of(&c["pitch"], 1.)).product::<f32>();
    let bus = chain.iter().find_map(|(n, _)| match n.as_str() { "Music" => Some(Bus::Music), "SFX" => Some(Bus::Sfx), "Voice" => Some(Bus::Voice), "Fort_01_VOIP" => Some(Bus::VoiceChat), "Master" => Some(Bus::Master), _ => None }).unwrap_or(Bus::Sfx);
    ClassInfo { name: chain[0].0.clone(), base_volume, pitch, bus, is_ui: chain[0].1["ui"].as_bool().unwrap_or(false), always_play: chain[0].1["always"].as_bool().unwrap_or(false), chain: chain.iter().map(|(n, _)| n.to_lowercase()).collect() }
}

struct Ctx<'a> { atts: &'a Value, classes: &'a Value, att_cache: HashMap<String, Arc<Attenuation>>, class_cache: HashMap<String, ClassInfo> }
impl Ctx<'_> {
    fn att(&mut self, k: &str) -> Option<Arc<Attenuation>> {
        if let Some(a) = self.att_cache.get(k) { return Some(a.clone()); }
        let a = Arc::new(attenuation(self.atts.get(k)?));
        self.att_cache.insert(k.to_owned(), a.clone());
        Some(a)
    }
    fn class(&mut self, k: &str) -> ClassInfo { let classes = self.classes; self.class_cache.entry(k.to_owned()).or_insert_with(|| class_info(classes, k)).clone() }
}

fn curve(v: &Value) -> Curve { Curve(v.as_array().map(|a| a.iter().filter_map(|k| Some(Key { time: f32_of(k.get(0)?, 0.), value: f32_of(k.get(1)?, 0.), interp: k.get(2).and_then(Value::as_u64).unwrap_or(0) as u8, arrive_tangent: k.get(3).map_or(0., |x| f32_of(x, 0.)), leave_tangent: k.get(4).map_or(0., |x| f32_of(x, 0.)) })).collect()).unwrap_or_default()) }

fn floats(v: &Value) -> Vec<f32> { v.as_array().map(|a| a.iter().map(|x| f32_of(x, 1.)).collect()).unwrap_or_default() }
fn fades(v: &Value) -> Vec<FadeRange> { v.as_array().map(|a| a.iter().map(|d| FadeRange { fade_in_start: f32_of(&d["is"], 0.), fade_in_end: f32_of(&d["ie"], 0.), fade_out_start: f32_of(&d["os"], 0.), fade_out_end: f32_of(&d["oe"], 0.), volume: f32_of(&d["v"], 1.) }).collect()).unwrap_or_default() }

fn node(v: &Value, cx: &mut Ctx, depth: usize) -> CueNode {
    let children = if depth < 64 { v["c"].as_array().map(|a| a.iter().map(|c| node(c, cx, depth + 1)).collect()).unwrap_or_default() } else { vec![] };
    let s = |k: &str| v[k].as_str().unwrap_or("None").to_owned();
    let kind = match v["k"].as_str().unwrap_or("") {
        "wave" => NodeKind::WavePlayer { wave: v["wave"].as_str().map(str::to_owned), looping: v["loop"].as_bool().unwrap_or(false) },
        "random" => NodeKind::Random { weights: floats(&v["w"]), without_replacement: v["norep"].as_bool().unwrap_or(false) },
        "mixer" => NodeKind::Mixer { input_volume: floats(&v["v"]) },
        "concat" => NodeKind::Concatenator { input_volume: floats(&v["v"]) },
        "mod" => { let p = floats(&v["p"]); let q = floats(&v["v"]); NodeKind::Modulator { pitch: (p.first().copied().unwrap_or(0.95), p.get(1).copied().unwrap_or(1.05)), volume: (q.first().copied().unwrap_or(0.95), q.get(1).copied().unwrap_or(1.05)) } }
        "att" => NodeKind::Attenuation(v["att"].as_str().and_then(|k| cx.att(k))),
        "switch" => NodeKind::Switch { param: s("param") },
        "branch" => NodeKind::Branch { param: s("param") },
        "pxfade" => NodeKind::ParamCrossFade { param: s("param"), inputs: fades(&v["in"]) },
        "dxfade" => NodeKind::DistanceCrossFade { inputs: fades(&v["in"]) },
        "quality" => NodeKind::QualityLevel,
        "class" => match v["cls"].as_str() { Some(k) => NodeKind::SoundClass { class: k.rsplit('/').next().unwrap_or(k).to_owned(), info: Some(cx.class(k)) }, None => NodeKind::Mixer { input_volume: vec![] } },
        "loop" => NodeKind::Looping { count: v["n"].as_u64().unwrap_or(1) as u32, indefinite: v["inf"].as_bool().unwrap_or(true) },
        "delay" => NodeKind::Delay { min: f32_of(&v["min"], 0.), max: f32_of(&v["max"], 0.) },
        "modc" => NodeKind::ModulatorContinuous,
        "env" => NodeKind::Enveloper { volume: curve(&v["vol"]), pitch: curve(&v["pitch"]), looping: v["loop"].as_bool().unwrap_or(false), loop_start: f32_of(&v["ls"], 0.), loop_end: f32_of(&v["le"], 0.) },
        other => NodeKind::Unknown(v["cls"].as_str().unwrap_or(other).to_owned()),
    };
    CueNode { name: String::new(), kind, children }
}

fn row(r: &Value) -> Option<Row> {
    let kind = match r["k"].as_str()? { "lib" => Kind::Lib, "state" => Kind::State, "step" => Kind::Step, "gc" => Kind::Gc, "local" => Kind::Local, _ => return None };
    Some(Row {
        t: f32_of(&r["t"], 0.), kind, ev: r["ev"].as_str().map(str::to_owned), surf: r["surf"].as_u64().map(|x| x as u8), stance: r["stance"].as_i64().map(|x| x as i32),
        vol: f32_of(&r["vol"], 1.), pitch: f32_of(&r["pitch"], 1.), chance: f32_of(&r["chance"], 1.), thr: f32_of(&r["thr"], 1e-5), cue: r["cue"].as_str().map(str::to_owned), gc: r["gc"].as_str().map(str::to_owned),
    })
}

fn reachable_waves(v: &Value, out: &mut std::collections::HashSet<String>, depth: usize) {
    if depth > 64 { return; }
    if let Some(w) = v["wave"].as_str() { out.insert(w.to_owned()); }
    let Some(c) = v["c"].as_array() else { return };
    if v["k"].as_str() == Some("switch") && v["param"].as_str() == Some("PlayerRelation") {
        if let Some(local) = c.get(RELATION_LOCAL as usize + 1) { reachable_waves(local, out, depth + 1); }
        return;
    }
    for x in c { reachable_waves(x, out, depth + 1); }
}

fn load(folder: &Path) -> Result<Loaded, String> {
    let index: Value = serde_json::from_slice(&std::fs::read(folder.join("index.json")).map_err(|e| format!("index.json: {e}"))?).map_err(|e| format!("index.json: {e}"))?;
    let pack = std::fs::read(folder.join("waves.pack")).map_err(|e| format!("waves.pack: {e}"))?;
    let hand = fn_live::var("FN_ENH_HAND").ok().filter(|v| v == "bare").map_or("hand_glove", |_| "hand_bare").to_owned();
    let mut surfaces: std::collections::HashSet<&str> = PHYS_REACHABLE.iter().map(|p| SURFACE_NAMES[footstep_surface(*p) as usize]).collect();
    for rows in index["notifies"].as_object().into_iter().flat_map(|o| o.values()) { for r in rows.as_array().into_iter().flatten() { if let Some(s) = r["surf"].as_u64().and_then(|s| SURFACE_NAMES.get(s as usize).copied()) { surfaces.insert(s); } } }
    let mut needed_cues: std::collections::HashSet<String> = std::collections::HashSet::new();
    for key in ["footsteps", "clothing", hand.as_str()] {
        for (tag, cue) in index["libraries"][key]["tags"].as_object().into_iter().flatten() {
            let tail = tag.rsplit('.').next().unwrap_or("");
            let surfaced = SURFACE_NAMES.contains(&tail) || ["Carpet", "Puddle", "ForestCanopy", "Clothing"].contains(&tail);
            if !surfaced || surfaces.contains(tail) { if let Some(c) = cue.as_str() { needed_cues.insert(c.to_owned()); } }
        }
    }
    for g in index["gameplay_cues"].as_object().into_iter().flat_map(|o| o.values()) {
        for b in g["burst"].as_array().into_iter().flatten() { if let Some(c) = b["cue"].as_str() { needed_cues.insert(c.to_owned()); } }
        for k in ["start", "stop"] { if let Some(c) = g[k].as_str() { needed_cues.insert(c.to_owned()); } }
        if let Some(c) = g["loop"]["cue"].as_str() { needed_cues.insert(c.to_owned()); }
    }
    for rows in index["notifies"].as_object().into_iter().flat_map(|o| o.values()) { for r in rows.as_array().into_iter().flatten() { if let Some(c) = r["cue"].as_str() { needed_cues.insert(c.to_owned()); } } }
    let mut needed_waves = std::collections::HashSet::new();
    for c in &needed_cues { if let Some(cv) = index["cues"].get(c) { reachable_waves(&cv["root"], &mut needed_waves, 0); } }
    let mut waves = vec![];
    let mut pcm_bytes = 0;
    for (k, w) in index["waves"].as_object().ok_or("index.json: no waves")? {
        if !needed_waves.contains(k) { continue; }
        let (o, n) = (w["offset"].as_u64().unwrap_or(0) as usize, w["size"].as_u64().unwrap_or(0) as usize);
        let Some(bytes) = pack.get(o..o + n) else { eprintln!("[enhanced-sound] {k}: outside waves.pack"); continue };
        let mut pcm = match fn_audio::codec::ogg::decode_ogg(bytes) { Ok(p) => p, Err(e) => { eprintln!("[enhanced-sound] {k}: {e}"); continue } };
        if w["looping"].as_bool().unwrap_or(false) { pcm.loop_start = Some(0); pcm.loop_end = Some(pcm.frames() as u64); }
        let mut lw = LoadedWave::new(pcm);
        lw.volume = f32_of(&w["volume"], 1.);
        lw.pitch = f32_of(&w["pitch"], 1.);
        let lw = lw.compact();
        pcm_bytes += lw.pcm16.len() * 2;
        waves.push((k.clone(), Arc::new(lw)));
    }
    let mut cx = Ctx { atts: &index["attenuations"], classes: &index["classes"], att_cache: HashMap::new(), class_cache: HashMap::new() };
    let mut cues = vec![];
    for (k, c) in index["cues"].as_object().ok_or("index.json: no cues")? {
        if !needed_cues.contains(k) { continue; }
        let root = (!c["root"].is_null()).then(|| node(&c["root"], &mut cx, 0));
        let class_name = c["class"].as_str().map(|s| s.rsplit('/').next().unwrap_or(s).to_owned());
        let class = c["class"].as_str().map_or_else(ClassInfo::default, |s| cx.class(s));
        let attenuation = c["att"].as_str().and_then(|a| cx.att(a));
        cues.push((k.clone(), Arc::new(SoundCue { name: c["name"].as_str().unwrap_or("").to_owned(), path: k.clone(), root, volume_multiplier: f32_of(&c["vol"], 1.), pitch_multiplier: f32_of(&c["pitch"], 1.), priority: f32_of(&c["priority"], 1.), duration: f32_of(&c["duration"], 0.), class_name, class, attenuation, concurrency: None })));
    }
    let mut libs = vec![];
    for key in ["footsteps", "clothing", hand.as_str()] {
        if let Some(l) = index["libraries"].get(key) {
            let m: HashMap<String, String> = l["tags"].as_object().map(|o| o.iter().filter_map(|(t, c)| Some((t.clone(), c.as_str()?.to_owned()))).collect()).unwrap_or_default();
            libs.push((key.to_owned(), m));
        }
    }
    let notifies = index["notifies"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_array().map(|a| a.iter().filter_map(row).collect()).unwrap_or_default())).collect()).unwrap_or_default();
    let mut gcs = HashMap::new();
    if let Some(o) = index["gameplay_cues"].as_object() {
        for (tag, g) in o {
            let burst = g["burst"].as_array().map(|a| a.iter().filter_map(|b| Some(Burst { cue: b["cue"].as_str()?.to_owned(), delay: f32_of(&b["delay"], 0.), chance: f32_of(&b["chance"], 1.) })).collect()).unwrap_or_default();
            let params = g["loop"]["params"].as_array().map(|a| a.iter().filter_map(|p| Some(LoopParam { name: p["name"].as_str()?.to_owned(), attack: f32_of(&p["attack"], 0.), release: f32_of(&p["release"], 0.) })).collect()).unwrap_or_default();
            gcs.insert(tag.clone(), Gc { burst, start: g["start"].as_str().map(str::to_owned), stop: g["stop"].as_str().map(str::to_owned), loop_cue: g["loop"]["cue"].as_str().map(str::to_owned), params });
        }
    }
    Ok(Loaded { meta: Meta { libs, notifies, gcs, hand }, waves, cues, pcm_bytes })
}

fn rand01(s: &mut u64) -> f32 { *s ^= *s << 13; *s ^= *s >> 7; *s ^= *s << 17; (*s >> 40) as f32 / (1u64 << 24) as f32 }

fn feet_surface_phys(sim: &GameSim) -> u32 {
    let feet = sim.feet();
    let origin = feet + Vec3::Y * SURFACE_TRACE_UP_M;
    if let Some(w) = fn_terrain::world() { if crate::water_hook::fluid_friction_at(w.map.to_ue(feet)) != 1. { return PHYS_WATER; } }
    let prop = fn_hitreg::shoot::trace_props(&sim.world.props, origin, Vec3::NEG_Y, SURFACE_TRACE_LEN_M).map(|(i, t, _)| (t, fn_hitreg::surface::of_prop(&sim.world.props[i])));
    let bo = Vec3::new(origin.x * 100., (origin.z - fn_game::sim::Z_OFFSET) * 100., origin.y * 100.);
    let piece = crate::building_hook::with_world(|w| fn_building::damage::trace(w, bo, Vec3::NEG_Z, SURFACE_TRACE_LEN_M * 100., 0.).and_then(|(id, t, _)| w.get(id).map(|p| (t * 0.01, match p.material { fn_building::pieces::Material::Wood => 1, fn_building::pieces::Material::Stone => 2, fn_building::pieces::Material::Metal => 3 }))));
    match (prop, piece) { (Some(a), Some(b)) => if a.0 <= b.0 { a.1 } else { b.1 }, (Some(a), None) => a.1, (None, Some(b)) => b.1, (None, None) => 0 }
}

fn footstep_surface(phys: u32) -> u8 { PHYS_TO_FOOTSTEP.iter().find(|(p, _)| *p == phys).map_or(0, |x| x.1) }

fn stance(sim: &GameSim) -> i32 {
    let c = sim.controller.component();
    let moving = c.velocity.truncate().length() > 1.;
    if sim.anim.enh.tactical_weight() > 0.5 { return STANCE_SPRINT; }
    if c.is_crouching() { return if sim.anim.vars.b_is_sprinting { STANCE_CROUCH_RUN } else { STANCE_CROUCH_WALK }; }
    if sim.anim.vars.b_is_sprinting { STANCE_SPRINT } else if moving { STANCE_RUN } else { STANCE_WALK }
}

fn find_cue<'a>(lib: &'a HashMap<String, String>, ev: &str, surf: &str) -> Option<&'a String> { lib.get(&format!("{ev}.{surf}")).or_else(|| lib.get(ev)) }

#[derive(Clone, Copy)]
struct Emit { pos: Vec3, relation: i32, stance: i32, elevation: f32, pitch: Option<f32> }

fn play_event(sink: &mut dyn AudioSink, meta: &Meta, ev: &str, surf: Option<u8>, vol: f32, pitch: f32, e: &Emit, pawn_surface: &mut dyn FnMut() -> u8) {
    let s = match surf.filter(|s| *s != SURFACE_MAX_NONE) { Some(s) => s, None => pawn_surface() } as usize;
    let name = SURFACE_NAMES.get(s).copied().unwrap_or(SURFACE_NAMES[0]);
    let p = e.pitch.unwrap_or(pitch);
    for (_, lib) in &meta.libs {
        if let Some(cue) = find_cue(lib, ev, name) {
            sink.play_baked(cue, Some(e.pos), vol, p, &[("PlayerRelation", e.relation), ("Stance", e.stance)], &[("Elevation", e.elevation + 2.)]);
        }
    }
}

pub fn elevation_and_pitch(source_z_cm: f32, listener_z_cm: f32) -> (f32, f32) {
    let dz = source_z_cm - listener_z_cm;
    let range = ELEVATION_TILE_RANGE * ELEVATION_TILE_CM;
    let f = ((dz + range) / (2. * range)).clamp(0., 1.);
    let elev = 2. * f - 1.;
    let dead = PITCH_DEADZONE_PCT * ELEVATION_TILE_CM;
    let pitch = if dz > dead { (PITCH_MAX_ABOVE - 1.) * elev + 1. } else if dz >= -dead { 1. } else { (1. - PITCH_MAX_BELOW) * (2. * f) + PITCH_MAX_BELOW };
    (elev, pitch)
}

fn fire_rows(sink: &mut dyn AudioSink, meta: &Meta, rows: &[Row], from: f32, to: f32, weight: f32, e: &Emit, st: &mut State, pawn_surface: &mut dyn FnMut() -> u8) {
    for r in rows.iter().filter(|r| r.t > from && r.t <= to && weight > r.thr) {
        if r.chance < 1. && rand01(&mut st.rng) >= r.chance { continue; }
        match r.kind {
            Kind::Lib | Kind::State | Kind::Step => {
                let ev = r.ev.as_deref().unwrap_or(EV_STEP);
                let es = Emit { stance: r.stance.filter(|s| *s < 5).unwrap_or(e.stance), ..*e };
                play_event(sink, meta, ev, r.surf, r.vol, r.pitch, &es, pawn_surface);
            }
            Kind::Gc => if let Some(g) = r.gc.as_deref().and_then(|t| meta.gcs.get(t)) { burst(sink, g, e, st); },
            Kind::Local => if e.relation == RELATION_LOCAL { if let Some(c) = &r.cue { sink.play_baked(c, Some(e.pos), r.vol, r.pitch, &LOCAL_FLAGS, &[]); } },
        }
    }
}

fn burst(sink: &mut dyn AudioSink, g: &Gc, e: &Emit, st: &mut State) {
    for b in &g.burst {
        if b.chance < 1. && rand01(&mut st.rng) >= b.chance { continue; }
        if b.delay > 0. { st.delayed.push((st.clock + b.delay, b.cue.clone())); } else { sink.play_baked(&b.cue, Some(e.pos), 1., 1., &LOCAL_FLAGS, &[]); }
    }
}

fn ranges(prev: Option<f32>, t: f32, len: f32, looping: bool) -> [(f32, f32); 2] {
    let none = (0., -1.);
    let Some(p) = prev else { return [(-1e-6, if looping { t % len } else { t.min(len) }), none] };
    if looping {
        let (a, b) = (p % len, t % len);
        if t < p { return [(-1e-6, b), none]; }
        if b < a || t - p >= len { [(a, len), (-1e-6, b)] } else { [(a, b), none] }
    } else if t < p { [(-1e-6, t.min(len)), none] } else { [(p.min(len), t.min(len)), none] }
}

fn slide_frame(sink: &mut dyn AudioSink, meta: &Meta, sim: &GameSim, e: &Emit, dt: f32, st: &mut State) {
    let on = sim.anim.enh.input.enabled && sim.anim.enh.input.sliding;
    let Some(g) = meta.gcs.get(GC_SLIDING) else { return };
    let speed = sim.controller.velocity_cm().length();
    let target = |name: &str| match name { "VelocityLength" => ((speed - SLIDE_VELOCITY_RANGE.0) / (SLIDE_VELOCITY_RANGE.1 - SLIDE_VELOCITY_RANGE.0)).clamp(0., 1.), "ZVelocity" => ((speed - SLIDE_ZVELOCITY_RANGE.0) / (SLIDE_ZVELOCITY_RANGE.1 - SLIDE_ZVELOCITY_RANGE.0)).clamp(0., 1.), _ => 0. };
    let flags: [(&str, i32); 2] = [("IsLocal", (e.relation == RELATION_LOCAL) as i32), ("IsTeam", 1)];
    if on && !st.slide.on {
        st.slide.on = true;
        if let Some(c) = &g.start { sink.play_baked(c, Some(e.pos), 1., 1., &flags, &[]); }
        sink.fade_baked(st.slide.stop_id, None, 0., SLIDE_STOP_FADE_OUT, true);
        st.slide.stop_id = 0;
        st.slide.params = g.params.iter().map(|p| (p.name.clone(), 0.)).collect();
        st.slide.next_param = 0.;
        if let Some(c) = &g.loop_cue {
            let fl: Vec<(&str, f32)> = st.slide.params.iter().map(|(k, v)| (k.as_str(), *v)).collect();
            st.slide.loop_id = sink.play_baked(c, Some(e.pos), 1., 1., &flags, &fl);
            sink.fade_baked(st.slide.loop_id, Some(0.), 1., SLIDE_LOOP_FADE_IN, false);
        }
    } else if !on && st.slide.on {
        st.slide.on = false;
        sink.fade_baked(st.slide.loop_id, None, 0., SLIDE_LOOP_FADE_OUT, true);
        st.slide.loop_id = 0;
        if let Some(c) = &g.stop { st.slide.stop_id = sink.play_baked(c, Some(e.pos), 1., 1., &flags[..1], &[]); }
    }
    if st.slide.on && st.slide.loop_id != 0 {
        st.slide.next_param -= dt;
        let refresh = st.slide.next_param <= 0.;
        if refresh { st.slide.next_param += SLIDE_PARAM_PERIOD; }
        for p in &g.params {
            let goal = target(&p.name);
            let v = st.slide.params.entry(p.name.clone()).or_insert(0.);
            let speed_k = if goal > *v { p.attack } else { p.release };
            if refresh || speed_k > 0. { *v += (goal - *v) * (dt * speed_k).clamp(0., 1.); }
        }
        let fl: Vec<(&str, f32)> = st.slide.params.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        sink.adjust_baked(st.slide.loop_id, Some(e.pos), &fl);
    }
}

pub fn frame(sink: &mut dyn AudioSink, sim: &GameSim, dt: f32) {
    with(|st| {
        receive(st);
        if let Some((w, c)) = st.pending.take() { sink.add_baked(w, c); }
        let Some(meta) = st.meta.clone() else { return };
        st.clock += dt.max(0.);
        let enh = &sim.anim.enh;
        let feet = sim.feet();
        let mut e = Emit { pos: feet, relation: RELATION_LOCAL, stance: STANCE_WALK, elevation: 0., pitch: None };
        let due: Vec<String> = { let now = st.clock; let (d, keep): (Vec<_>, Vec<_>) = st.delayed.drain(..).partition(|(t, _)| *t <= now); st.delayed = keep; d.into_iter().map(|x| x.1).collect() };
        for c in due { sink.play_baked(&c, Some(feet), 1., 1., &LOCAL_FLAGS, &[]); }
        slide_frame(sink, &meta, sim, &e, dt, st);
        if !enh.input.enabled { st.prev.clear(); st.montage = None; return; }
        if enh.steps == st.last_steps { return; }
        st.last_steps = enh.steps;
        if !enh.active() { st.prev.clear(); st.montage = None; return; }
        e.stance = stance(sim);
        let mut cached: Option<u8> = None;
        let mut pawn_surface = || *cached.get_or_insert_with(|| footstep_surface(feet_surface_phys(sim)));
        let mut tracks = std::mem::take(&mut st.tracks);
        enh.notify_tracks(&mut tracks);
        let mut seen = HashMap::new();
        for tr in &tracks {
            let len = ea::clip_length(&ea::key(&tr.name)).max(1e-3);
            let key = (tr.slot, tr.name.clone());
            let prev = st.prev.get(&key).copied();
            seen.insert(key, tr.time);
            let Some(rows) = meta.notifies.get(&tr.name) else { continue };
            for (a, b) in ranges(prev, tr.time, len, tr.looping) { if b > a { fire_rows(sink, &meta, rows, a, b, tr.weight, &e, st, &mut pawn_surface); } }
        }
        st.prev = seen;
        st.tracks = tracks;
        let m = enh.montage_notify_ranges();
        let name = m.as_ref().map(|x| x.0);
        if name != st.montage && name.is_some_and(|n| n.starts_with("Clamber")) { if let Some(g) = meta.gcs.get(GC_CLAMBER) { burst(sink, g, &e, st); } }
        st.montage = name;
        if let Some((n, rs, w)) = m { if let Some(rows) = meta.notifies.get(n) { for (a, b) in rs { if b > a { fire_rows(sink, &meta, rows, a, b, w, &e, st, &mut pawn_surface); } } } }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn elevation_matches_the_footstep_cvars() {
        assert_eq!(elevation_and_pitch(0., 0.), (0., 1.));
        let (e, p) = elevation_and_pitch(1152., 0.);
        assert!((e - 1.).abs() < 1e-6 && (p - 1.2).abs() < 1e-6);
        let (e, p) = elevation_and_pitch(-1152., 0.);
        assert!((e + 1.).abs() < 1e-6 && (p - 0.8).abs() < 1e-6);
        let (_, p) = elevation_and_pitch(300., 0.);
        assert_eq!(p, 1.);
    }
    #[test]
    fn notify_ranges_wrap_and_restart() {
        assert_eq!(ranges(Some(0.9), 1.1, 1.0, true)[0], (0.9, 1.0));
        assert!((ranges(Some(0.9), 1.1, 1.0, true)[1].1 - 0.1).abs() < 1e-5);
        assert_eq!(ranges(None, 0.05, 1.0, false)[0], (-1e-6, 0.05));
        assert_eq!(ranges(Some(0.5), 0.1, 1.0, false)[0], (-1e-6, 0.1));
    }
    #[test]
    fn physical_surfaces_map_like_the_footstep_component() {
        assert_eq!(SURFACE_NAMES[footstep_surface(1) as usize], "Wood");
        assert_eq!(SURFACE_NAMES[footstep_surface(2) as usize], "Stone");
        assert_eq!(SURFACE_NAMES[footstep_surface(3) as usize], "Metal");
        assert_eq!(SURFACE_NAMES[footstep_surface(0) as usize], "Dirt");
        assert_eq!(SURFACE_NAMES[footstep_surface(10) as usize], "Dirt");
    }
}
