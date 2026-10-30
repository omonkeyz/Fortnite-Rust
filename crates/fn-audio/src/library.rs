//! Asset library: loads waves, cues, attenuation/concurrency/class assets from an install with caching, and feeds an
//! [`EngineHandle`](crate::mixer::EngineHandle).

use crate::attenuation::Attenuation;
use crate::cue::{parse_cue, Concurrency, CueNode, CueResolver, NodeKind, SoundCue};
use crate::error::Result;
use crate::install::{read_ubulk, to_pak_asset_path, Install};
use crate::mixer::{asset_key, EngineHandle, LoadedWave};
use crate::soundclass::{SoundClassTree, SoundMix};
use crate::stream::{StreamSource, StreamedWave};
use fn_uasset::{PackageData, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

pub struct Library {
    pub install: Install,
    classes: Option<SoundClassTree>,
    waves: HashMap<String, Arc<LoadedWave>>,
    cues: HashMap<String, Arc<SoundCue>>,
    att_cache: HashMap<String, Option<Attenuation>>,
    conc_cache: HashMap<String, Option<Concurrency>>,
    dialogue: HashMap<String, Option<String>>,
    /// Per engine (handle id): which waves / cues were already queued.
    sent: HashMap<usize, (HashSet<String>, HashSet<String>)>,
    /// Non-fatal problems met while preparing cues (missing/undecodable waves, ...): `(asset, message)`.
    pub warnings: Vec<(String, String)>,
    /// `Some(s)`: waves of at least `s` seconds in a streamable format (Opus, Ogg) are decoded incrementally by a
    /// [`crate::stream::StreamedWave`] instead of whole (long looping ambience / music). `None` (default): decode whole.
    /// Waves already cached keep the form they were loaded in.
    pub stream_min_seconds: Option<f64>,
    pub compact_pcm: bool,
}

struct LibResolver<'a> {
    install: &'a Install,
    att: &'a mut HashMap<String, Option<Attenuation>>,
    conc: &'a mut HashMap<String, Option<Concurrency>>,
}

fn load_pkg_first(install: &Install, path: &str, class: &str, f: impl FnOnce(&fn_uasset::Package, usize) -> Option<fn_uasset::PropertyList>) -> Option<fn_uasset::PropertyList> {
    let ap = to_pak_asset_path(path.split('.').next().unwrap_or(path));
    let data = PackageData::load(&install.paks, &ap).ok()?;
    let pkg = data.package().ok()?;
    let i = pkg.exports_of_class(class).next()?;
    f(&pkg, i)
}

/// `CurveFloat` asset -> its keys.
fn load_float_curve(install: &Install, path: &str) -> Option<crate::curve::Curve> {
    let p = load_pkg_first(install, path, "CurveFloat", |pkg, i| {
        let o = pkg.read_export(i).ok()?;
        match o.get("FloatCurve") {
            Some(Value::Struct { props, .. }) => Some(props.clone()),
            _ => None,
        }
    })?;
    p.get("Keys").and_then(Value::as_array).map(crate::curve::Curve::from_keys)
}

impl CueResolver for LibResolver<'_> {
    fn attenuation(&mut self, object_path: &str) -> Option<Attenuation> {
        let key = asset_key(object_path);
        if let Some(c) = self.att.get(&key) {
            return c.clone();
        }
        let v = (|| {
            let ap = to_pak_asset_path(object_path.split('.').next().unwrap_or(object_path));
            let data = PackageData::load(&self.install.paks, &ap).ok()?;
            let pkg = data.package().ok()?;
            let i = pkg.exports_of_class("SoundAttenuation").next()?;
            let o = pkg.read_export(i).ok()?;
            // an asset with only default settings stores no `Attenuation` property at all
            let props = match o.get("Attenuation") {
                Some(Value::Struct { props, .. }) => props.clone(),
                _ => fn_uasset::PropertyList::new(),
            };
            let mut a = Attenuation::from_props(&props);
            if a.algorithm == crate::attenuation::DistanceModel::Custom && a.custom_curve.is_empty() {
                if let Some(ext) = props.get("CustomAttenuationCurve").and_then(|c| c.field("ExternalCurve")).and_then(Value::as_object).filter(|i| !i.is_null()) {
                    a.custom_curve = load_float_curve(self.install, &pkg.object_path(ext)).unwrap_or_default();
                }
            }
            Some(a)
        })();
        self.att.insert(key, v.clone());
        v
    }    fn concurrency(&mut self, object_path: &str) -> Option<Concurrency> {
        let key = asset_key(object_path);
        if let Some(c) = self.conc.get(&key) {
            return c.clone();
        }
        let name = crate::soundclass::object_name(object_path).to_string();
        let v = load_pkg_first(self.install, object_path, "SoundConcurrency", |pkg, i| {
            let o = pkg.read_export(i).ok()?;
            match o.get("Concurrency") {
                Some(Value::Struct { props, .. }) => Some(props.clone()),
                _ => Some(fn_uasset::PropertyList::new()),
            }
        })
        .map(|p| Concurrency::from_props(&name, &p));
        self.conc.insert(key, v.clone());
        v
    }
}

fn encoded_ok(a: &crate::soundwave::SoundWaveAsset) -> bool {
    if a.info.sample_rate.is_none() || a.info.channels.is_none() { return false; }
    if a.streamed.as_ref().is_some_and(|s| s.format == "OPUS") { return true; }
    a.streamed.is_none() && a.primary().is_some_and(|f| f.name == "OGG" || f.name == "OPUS")
}

impl Library {
    pub fn open(root: impl AsRef<Path>) -> Result<Library> {
        Ok(Library::new(Install::open(root)?))
    }

    pub fn new(install: Install) -> Library {
        Library {
            install,
            classes: None,
            waves: HashMap::new(),
            cues: HashMap::new(),
            att_cache: HashMap::new(),
            conc_cache: HashMap::new(),
            dialogue: HashMap::new(),
            sent: HashMap::new(),
            compact_pcm: cfg!(target_arch = "wasm32"),
            warnings: Vec::new(),
            stream_min_seconds: None,
        }
    }

    /// The SoundClass tree (loaded on first use).
    pub fn classes(&mut self) -> &SoundClassTree {
        if self.classes.is_none() {
            self.classes = Some(SoundClassTree::load(&self.install).unwrap_or_default());
        }
        self.classes.as_ref().unwrap()
    }

    pub fn load_wave(&mut self, path: &str) -> Result<Arc<LoadedWave>> {
        let key = asset_key(path);
        if let Some(w) = self.waves.get(&key) {
            return Ok(w.clone());
        }
        let asset = self.install.load_soundwave(path.split('.').next().unwrap_or(path))?;
        let mut streamed = None;
        if let Some(min) = self.stream_min_seconds {
            // long Opus / Ogg waves are decoded incrementally on a worker thread instead of whole (see crate::stream);
            // anything the streamer cannot take (other formats, no granule position, > 2 channels) is decoded whole
            if let Ok(Some(src)) = StreamSource::from_asset(&asset) {
                if src.duration_seconds() >= min {
                    streamed = StreamedWave::new(Arc::new(src)).ok();
                }
            }
        }
        let w = match streamed {
            Some(s) => {
                let mut lw = LoadedWave::streamed(s, asset.info.looping);
                lw.volume = asset.info.volume.unwrap_or(1.0);
                lw.pitch = asset.info.pitch.unwrap_or(1.0);
                Arc::new(lw)
            }
            None if self.compact_pcm && encoded_ok(&asset) => {
                let mut a = asset;
                let keep = a.primary().cloned();
                a.formats = keep.into_iter().collect();
                let (v, p) = (a.info.volume.unwrap_or(1.0), a.info.pitch.unwrap_or(1.0));
                Arc::new(LoadedWave::encoded(a, v, p))
            }
            None => {
                let pcm = crate::codec::decode_asset(&asset)?;
                { let lw = LoadedWave { pcm, volume: asset.info.volume.unwrap_or(1.0), pitch: asset.info.pitch.unwrap_or(1.0), stream: None, pcm16: Vec::new(), lazy: None }; Arc::new(if self.compact_pcm { lw.compact() } else { lw }) }
            }
        };
        self.waves.insert(key, w.clone());
        Ok(w)
    }

    /// `DialogueWave` -> long path of the SoundWave of its first context mapping.
    pub fn resolve_dialogue(&mut self, path: &str) -> Option<String> {
        let key = asset_key(path);
        if let Some(r) = self.dialogue.get(&key) {
            return r.clone();
        }
        let r = (|| {
            let ap = to_pak_asset_path(path.split('.').next().unwrap_or(path));
            let data = PackageData::load(&self.install.paks, &ap).ok()?;
            let pkg = data.package().ok()?;
            let i = pkg.exports_of_class("DialogueWave").next()?;
            let o = pkg.read_export(i).ok()?;
            let maps = o.get("ContextMappings")?.as_array()?;
            let sw = maps.first()?.field("SoundWave")?.as_object().filter(|i| !i.is_null())?;
            Some(pkg.object_path(sw).split('.').next()?.to_string())
        })();
        self.dialogue.insert(key, r.clone());
        r
    }

    /// Parse a cue (cached), resolving attenuation / concurrency / SoundClass assets.
    pub fn load_cue(&mut self, path: &str) -> Result<Arc<SoundCue>> {
        let key = asset_key(path);
        if let Some(c) = self.cues.get(&key) {
            return Ok(c.clone());
        }
        let ap = to_pak_asset_path(path.split('.').next().unwrap_or(path));
        let data = PackageData::load(&self.install.paks, &ap)?;
        let pkg = data.package()?;
        let mut cue = {
            let mut r = LibResolver { install: &self.install, att: &mut self.att_cache, conc: &mut self.conc_cache };
            parse_cue(&pkg, &mut r)?
        };
        drop(pkg);
        // class resolution (cue + SoundClass nodes)
        let classes = self.classes().clone();
        cue.class = classes.info(cue.class_name.as_deref().unwrap_or("SFX"));
        fn fix(n: &mut CueNode, classes: &SoundClassTree) {
            if let NodeKind::SoundClass { class, info } = &mut n.kind {
                if !class.is_empty() {
                    *info = Some(classes.info(class));
                }
            }
            for c in &mut n.children {
                fix(c, classes);
            }
        }
        if let Some(r) = &mut cue.root {
            fix(r, &classes);
        }
        let cue = Arc::new(cue);
        self.cues.insert(key, cue.clone());
        Ok(cue)
    }

    pub fn load_mix(&mut self, path: &str) -> Result<Option<SoundMix>> {
        SoundMix::load(&self.install, path)
    }

    /// Make a cue playable: parse it, decode every wave it references and queue AddWave/AddCue on the engine.
    /// Returns the number of waves that could not be loaded (they are also listed in `warnings`).
    pub fn prepare_cue(&mut self, engine: &EngineHandle, path: &str) -> Result<usize> {
        let cue = self.load_cue(path)?;
        let mut missing = 0;
        let (mut sent_waves, mut sent_cues) = self.sent.remove(&engine.id()).unwrap_or_default();
        let mut wave_paths = cue.wave_paths();
        // dialogue nodes
        let mut dialogue = Vec::new();
        if let Some(r) = &cue.root {
            r.walk(&mut |n| {
                if let NodeKind::Dialogue { dialogue_wave: Some(d) } = &n.kind {
                    dialogue.push(d.clone());
                }
            });
        }
        for d in dialogue {
            match self.resolve_dialogue(&d) {
                Some(w) => {
                    match self.load_wave(&w) {
                        Ok(lw) => {
                            if sent_waves.insert(asset_key(&d)) {
                                engine.add_wave_arc(&d, lw)?;
                            }
                        }
                        Err(e) => {
                            missing += 1;
                            self.warnings.push((w, e.to_string()));
                        }
                    }
                }
                None => {
                    missing += 1;
                    self.warnings.push((d, "dialogue wave has no sound wave".into()));
                }
            }
        }
        wave_paths.retain(|w| !sent_waves.contains(&asset_key(w)));
        for w in wave_paths {
            match self.load_wave(&w) {
                Ok(lw) => {
                    sent_waves.insert(asset_key(&w));
                    engine.add_wave_arc(&w, lw)?;
                }
                Err(e) => {
                    missing += 1;
                    self.warnings.push((w, e.to_string()));
                }
            }
        }
        if sent_cues.insert(asset_key(path)) {
            engine.add_cue(path, cue)?;
        }
        self.sent.insert(engine.id(), (sent_waves, sent_cues));
        Ok(missing)
    }

    /// Counters (underruns, decoded frames, loop wraps, ...) of every wave held in streamed form, by asset key.
    pub fn stream_stats(&self) -> Vec<(String, crate::stream::StreamStatsSnapshot)> {
        let mut v: Vec<_> = self.waves.iter().filter_map(|(k, w)| w.stream.as_ref().map(|s| (k.clone(), s.stats()))).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Whether the wave `path` is held in streamed form.
    pub fn is_streamed(&self, path: &str) -> bool {
        self.waves.get(&asset_key(path)).map_or(false, |w| w.stream.is_some())
    }

    /// Number of decoded waves held (cache size).
    pub fn cached_waves(&self) -> usize {
        self.waves.len()
    }

    /// Read the `.ubulk`-less package bytes of anything (re-exported helper for tools).
    pub fn ubulk_of(&self, asset_path: &str) -> Vec<u8> {
        read_ubulk(&self.install.paks, asset_path)
    }
}



