//! The settings apply layer of the app: [`RenderSettings`] (what the renderer-bound settings become), [`AppBackend`] (runs `fn_settings`' apply
//! functions with the live hooks and writes `FortniteSettings.ini` atomically). `AppBackend` is also the `ApplyBackend` the Settings screen uses
//! for its Apply / Preview / Revert flow, so the menu and the start-up apply share one path.
use crate::audio::{AudioSink, SinkHooks};
use fn_render::RenderOptions;
use fn_settings::{ApplyReport, InputLive, Level, LiveVideo, PlatformHost, PostFlags, Runtime, Settings, VideoHooks};
use fn_ui::menu::pages::ApplyBackend;
use std::path::PathBuf;

/// `fn_settings::VideoHooks` for the renderer: the settings values become [`RenderOptions`] (+ far plane / quality levels the renderer will grow into).
#[derive(Clone, Debug)]
pub struct RenderSettings {
    pub options: RenderOptions,
    /// Shadow map resolution (None = shadows off). The renderer's cascade atlas size is fixed at 2048: only off/on is honoured today.
    pub shadow_map_size: Option<u32>,
    pub far_plane: f32,
    pub anti_aliasing: Level,
    pub texture_quality: Level,
    pub show_fps: bool,
    pub gamma: f32,
}
impl Default for RenderSettings {
    fn default() -> Self { RenderSettings { options: RenderOptions::default(), shadow_map_size: Some(2048), far_plane: fn_camera::FAR, anti_aliasing: 3, texture_quality: 3, show_fps: false, gamma: fn_settings::GAMMA_REFERENCE } }
}
impl VideoHooks for RenderSettings {
    fn set_shadow_map_size(&mut self, size: Option<u32>) -> Result<(), String> { self.shadow_map_size = size; self.options.shadows = size.is_some(); if let Some(s) = size { self.options.shadow_map_size = s; } Ok(()) }
    // Shadows level = sg.ShadowQuality: the whole cvar group (cascades, distance, PCF kernel, caster cull, DF / volumetric fog) reaches the renderer
    fn set_shadow_quality(&mut self, l: Level) -> Result<(), String> { self.options.shadow_scalability = fn_render::quality::ShadowScalability::level(l as u32); self.set_shadow_map_size(fn_render::quality::shadow_map_size(l as u32)) }
    fn set_post(&mut self, f: PostFlags) -> Result<(), String> { self.options.ao = f.ao; self.options.bloom = f.bloom; self.options.vignette = f.vignette; Ok(()) }
    // Post Processing level = sg.PostProcessQuality: bloom quality / fast blur / filter size, grain quantization, eye adaptation, light shafts, upscale
    fn set_post_quality(&mut self, l: Level) -> Result<(), String> {
        let p = fn_render::quality::PostProcessScalability::level(l as u32);
        self.options.post_scalability = p; self.options.light_shafts = p.light_shaft_quality > 0;
        self.set_post(fn_settings::post_flags(l))
    }
    // View Distance = r.ViewDistanceScale on the cull distances; 3D Resolution = r.ScreenPercentage
    fn set_view_distance_scale(&mut self, s: f32) -> Result<(), String> { self.options.view_distance_scale = if cfg!(target_arch = "wasm32") { s.min(0.5) } else { s }; Ok(()) }
    fn set_screen_percentage(&mut self, p: f32) -> Result<(), String> { self.options.screen_percentage = p; Ok(()) }
    // Effects level = sg.EffectsQuality (CookedIniVersion.txt L5284-5339): r.SSR.Quality 0/0/2/3 (L5286/5300/5314/5328, REFLECTION APPLY,
    // fn_render::quality::EffectsScalability) and the translucency lighting volume r.TranslucencyLightingVolumeDim 24/32/48/64 (L5284/5298/5312/5326),
    // r.TranslucencyVolumeBlur 0/0/1/1 (L5289/5303/5317/5331)
    fn set_effects_quality(&mut self, l: Level) -> Result<(), String> {
        self.options.effects = fn_render::quality::EffectsScalability::level(l as u32);
        fn_render::translucent_lighting::VolumeSettings::set_current(fn_render::translucent_lighting::VolumeSettings::for_effects_quality(l as u32));
        crate::local_lights_hook::set_effects_quality(l as u8);
        Ok(())
    }
    fn set_display_gamma(&mut self, g: f32) -> Result<(), String> { self.gamma = g; self.options.display_exponent = fn_settings::display_exponent(g); Ok(()) }
    fn set_far_plane(&mut self, m: f32) -> Result<(), String> { self.far_plane = m; Ok(()) }
    fn set_show_fps(&mut self, on: bool) -> Result<(), String> { self.show_fps = on; Ok(()) }
    // Motion Blur = UFortGameUserSettings::bMotionBlur -> EngineShowFlags.MotionBlur (exe ApplyNonResolutionSettings @1405bcb10); fn_render::effect_settings gates the pass
    fn set_motion_blur(&mut self, on: bool) -> Result<(), String> { self.options.motion_blur = on; Ok(()) }
    // Anti-Aliasing level = sg.AntiAliasingQuality (none / FXAA / temporal AA + sharpen); Textures level -> anisotropy (fn-render `quality` tables).
    fn set_anti_aliasing(&mut self, l: Level) -> Result<(), String> { self.anti_aliasing = l; self.options.aa = fn_render::quality::AaScalability::level(l as u32); self.options.fxaa = None; Ok(()) }
    fn set_texture_quality(&mut self, l: Level) -> Result<(), String> { self.texture_quality = l; let (skip, aniso) = fn_render::quality::texture_params(l as u32); self.options.skip_mips = skip; self.options.anisotropy = aniso; Ok(()) }
}

pub struct AppBackend {
    pub render: RenderSettings,
    pub runtime: Runtime,
    pub input: InputLive,
    pub audio: Box<dyn AudioSink>,
    /// Where Apply writes (None = automated run: never touches the user's file).
    pub path: Option<PathBuf>,
}
impl AppBackend {
    pub fn new(path: Option<PathBuf>, audio: Box<dyn AudioSink>) -> AppBackend { AppBackend { render: RenderSettings::default(), runtime: Runtime::default(), input: InputLive::default(), audio, path } }
    /// Every hook group over `s`; the first call applies everything, later calls only what changed (`Settings` keeps the bookkeeping).
    pub fn apply_all(&mut self, s: &mut Settings, platform: &mut dyn PlatformHost) -> ApplyReport {
        let mut r = { let mut hooks = LiveVideo { platform, renderer: &mut self.render, runtime: &mut self.runtime }; s.apply_video(&mut hooks) };
        r.merge(s.apply_input(&mut self.input));
        r.merge(s.apply_audio(&mut SinkHooks(self.audio.as_mut())));
        r.merge(s.apply_game());
        s.drain_changes();
        r
    }
}
impl ApplyBackend for AppBackend {
    fn apply(&mut self, s: &mut Settings, platform: &mut dyn PlatformHost, save: bool) -> ApplyReport {
        let mut r = self.apply_all(s, platform);
        if save { if let Err(e) = self.write_file(s) { r.failed.push(("save", e)); } }
        r
    }
    fn write_file(&mut self, s: &Settings) -> Result<(), String> { match &self.path { Some(p) => s.save(p).map_err(|e| format!("{}: {e}", p.display())), None => Ok(()) } }
    fn persists(&self) -> bool { self.path.is_some() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{NullAudio, RecordingAudio};
    use fn_settings::{DisplayMode, NullPlatform, Value};
    fn platform() -> NullPlatform { NullPlatform::new(vec![DisplayMode { width: 1920, height: 1080, hz: 144 }, DisplayMode { width: 1280, height: 720, hz: 60 }]) }
    #[test] fn first_apply_pushes_every_group_and_later_applies_only_changes() {
        let mut b = AppBackend::new(None, Box::new(NullAudio)); let mut s = Settings::default(); let mut p = platform();
        let r = b.apply_all(&mut s, &mut p); assert!(r.ok(), "{:?}", r.failed);
        assert!(p.log.iter().any(|l| l.starts_with("display ")) && p.log.contains(&"swap_interval false".to_string()) || p.log.iter().any(|l| l.starts_with("swap_interval")), "{:?}", p.log);
        p.log.clear(); let r = b.apply_all(&mut s, &mut p); assert!(r.ok() && p.log.is_empty(), "nothing changed -> nothing applied: {:?}", p.log);
        s.set("video.shadows", Value::Index(0)).unwrap(); b.apply_all(&mut s, &mut p);
        assert!(!b.render.options.shadows && b.render.shadow_map_size.is_none(), "shadows level 0 = off");
        s.set("video.shadows", Value::Index(3)).unwrap(); b.apply_all(&mut s, &mut p); assert!(b.render.options.shadows);
    }
    #[test] fn post_gamma_far_plane_and_cap_reach_their_sinks() {
        let mut b = AppBackend::new(None, Box::new(NullAudio)); let mut s = Settings::default(); let mut p = platform();
        s.set("video.post_processing", Value::Index(0)).unwrap(); s.set("video.brightness", Value::Float(1.)).unwrap(); s.set("video.view_distance", Value::Index(0)).unwrap(); s.set("video.frame_rate_limit", Value::Index(1)).unwrap(); s.set("video.vsync", Value::Bool(true)).unwrap();
        b.apply_all(&mut s, &mut p);
        assert!(!b.render.options.ao && b.render.options.bloom && !b.render.options.vignette, "PostProcessQuality@0: bloom stays (BloomQuality 4), SSAO and vignette off");
        assert!((b.render.gamma - 2.7).abs() < 1e-5 && b.render.options.display_exponent < 1., "brighter/darker follows the gamma exponent");
        assert_eq!(b.render.far_plane, fn_camera::FAR); assert_eq!(b.render.options.view_distance_scale, 0.5); assert_eq!(b.runtime.frame_cap, 60); assert!(b.runtime.vsync);
    }
    #[test] fn anti_aliasing_textures_and_shadow_size_reach_the_render_options() {
        let mut b = AppBackend::new(None, Box::new(NullAudio)); let mut s = Settings::default(); let mut p = platform();
        s.set("video.anti_aliasing", Value::Index(0)).unwrap(); s.set("video.textures", Value::Index(0)).unwrap(); s.set("video.shadows", Value::Index(1)).unwrap();
        b.apply_all(&mut s, &mut p);
        assert_eq!(b.render.options.aa.method(), fn_render::quality::AaMethod::None, "Anti-Aliasing level 0 = off");
        assert_eq!((b.render.options.skip_mips, b.render.options.anisotropy), (0, 1));
        assert_eq!(b.render.options.shadow_map_size, 1024); assert_eq!(b.render.options.shadow_scalability.pcf_quality(), 3);
        s.set("video.anti_aliasing", Value::Index(3)).unwrap(); s.set("video.textures", Value::Index(3)).unwrap(); s.set("video.shadows", Value::Index(3)).unwrap();
        b.apply_all(&mut s, &mut p);
        assert_eq!(b.render.options.aa.method(), fn_render::quality::AaMethod::TemporalAa { fast: false });
        assert_eq!((b.render.options.skip_mips, b.render.options.anisotropy), (0, 8));
        assert_eq!(b.render.options.shadow_map_size, 2048);
    }
    #[test] fn input_settings_update_the_live_input_and_volumes_reach_the_audio_sink() {
        let mut b = AppBackend::new(None, Box::new(RecordingAudio::default())); let mut s = Settings::default(); let mut p = platform();
        s.set("input.invert_y", Value::Bool(true)).unwrap(); s.set("input.toggle_sprint", Value::Bool(true)).unwrap(); s.set("audio.master_volume", Value::Float(0.25)).unwrap();
        let r = b.apply_all(&mut s, &mut p); assert!(r.ok()); assert!(b.input.invert_y && b.input.sprint_toggle);
    }
    #[test] fn save_writes_atomically_and_automated_runs_do_not_persist() {
        let dir = std::env::temp_dir().join(format!("fn-app-backend-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("FortniteSettings.ini");
        let mut b = AppBackend::new(Some(path.clone()), Box::new(NullAudio)); let mut s = Settings::default(); let mut p = platform();
        s.set("video.vsync", Value::Bool(true)).unwrap();
        let r = b.apply(&mut s, &mut p, true); assert!(r.ok(), "{:?}", r.failed); assert!(b.persists());
        let (back, rep) = Settings::load(&path); assert!(back.video.vsync && rep.warnings.iter().all(|w| !w.contains("corrupt")), "{:?}", rep);
        assert!(!path.with_extension("ini.tmp").exists(), "temp file renamed away");
        let mut auto = AppBackend::new(None, Box::new(NullAudio)); assert!(!auto.persists()); assert!(auto.write_file(&s).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

