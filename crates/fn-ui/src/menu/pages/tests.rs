//! Tests for the settings screen core (headless, real fonts, no GL). There are no globals: every test owns its `MenuSystem` (and with it the
//! settings `Session`) or a bare `Session` plus a mock game.
use super::*;
use crate::menu::pause::SettingsScreen;
use crate::menu::theme::*;
use crate::menu::*;
use crate::settings::{self, NullPlatform, Settings, Value};

fn fonts() -> Fonts { crate::testutil::fonts() }
const SIZES: [(f32, f32); 6] = [(1920., 1080.), (2560., 1080.), (1024., 768.), (640., 480.), (800., 1280.), (3440., 1440.)];

/// The game behind the menu: the real `Settings`, a platform with a fixed mode list and a recording apply backend that "persists".
struct Mock { s: Settings, platform: NullPlatform, be: NullBackend }
impl Mock {
    fn new() -> Self { let mut s = Settings::default(); s.drain_changes(); Mock { s, platform: NullPlatform::static_16_9(), be: NullBackend { persist: true, ..Default::default() } } }
    fn cx(&mut self) -> UiContext<'_> { UiContext::new(&mut self.s, &mut self.platform, &mut self.be) }
}

/// Headless menu with the generic pages, the Settings screen on top and a mock game behind the session.
struct H { m: MenuSystem, w: f32, h: f32, game: Mock }
impl H {
    fn new(w: f32, h: f32) -> Self {
        let game = Mock::new();
        let mut m = MenuSystem::new(fonts());
        register_generic_into(&mut m);
        m.fade_time = 0.;
        m.open();
        m.push(Box::new(SettingsScreen::new()));
        let mut s = H { m, w, h, game };
        s.frame(); s.frame();
        s
    }
    fn frame(&mut self) -> Vec<Cmd> { let c = self.m.frame_with(&mut self.game.cx(), self.w, self.h, 1. / 60.).cmds.clone(); c }
    fn frames(&mut self, n: usize) { for _ in 0..n { self.frame(); } }
    fn key(&mut self, k: NavKey) { self.m.event(Event::Nav(k)); self.frame(); }
    fn mv(&mut self, x: f32, y: f32) { self.m.event(Event::MouseMove(x, y)); self.frame(); }
    fn click(&mut self, x: f32, y: f32) { self.mv(x, y); self.m.event(Event::MouseDown(MouseButton::Left, x, y)); self.frame(); self.m.event(Event::MouseUp(MouseButton::Left, x, y)); self.frame(); self.frame(); }
    fn texts(&mut self) -> Vec<String> { self.frame().iter().filter_map(|c| if let Cmd::Text { text, .. } = c { Some(text.clone()) } else { None }).collect() }
    fn has(&mut self, s: &str) -> bool { self.texts().iter().any(|t| t == s) }
    fn tab(&mut self, i: usize) { self.m.set_settings_tab(i); self.frames(3); }
    fn pos_of(&mut self, label: &str) -> (f32, f32) {
        for c in self.frame() { if let Cmd::Text { x, top, box_h, text, .. } = c { if text == label { return (x + 4., top + box_h * 0.5); } } }
        panic!("no text {label:?}");
    }
    fn stack(&self) -> Vec<String> { self.m.stack_names() }
    fn page_index(&self, id: &str) -> usize { self.m.pages().iter().position(|p| p.id() == id).unwrap_or_else(|| panic!("page {id}")) }
}
// ---- layout / descriptors -----------------------------------------------------------------------------------------------
#[test]
fn layout_row_counts_match_original() {
    let game = layout_table::TABS.iter().find(|t| t.id == "game").unwrap();
    // 1.8: 21 Game rows (no Uncap Framerate) and 7 HUD rows (no Net Debug Stats)
    assert_eq!(game.subs[0].rows.iter().filter(|r| !r.extension).count(), 21);
    assert_eq!(game.subs[0].rows.iter().filter(|r| r.extension).map(|r| r.label).collect::<Vec<_>>(), Vec::<&str>::new());
    assert_eq!(game.subs[1].rows.len(), 7);
    let ids: Vec<&str> = layout_table::TABS.iter().map(|t| t.id).collect();
    assert_eq!(ids, ["video", "game", "brightness", "audio", "input", "controller", "livestream"]);
    let labels: Vec<&str> = layout_table::TABS.iter().map(|t| t.label).collect();
    assert_eq!(labels, ["Video", "Game", "Brightness", "Audio", "Input", "Controller", "Live Streaming"]);
    assert_eq!(game.subs.iter().map(|s| s.label).collect::<Vec<_>>(), ["Game", "HUD"]);
    let video = &layout_table::TABS[0];
    assert_eq!(video.subs[0].rows.len(), 15);
}
#[test]
fn every_bound_row_has_a_descriptor_and_a_label() {
    for t in rows::generic_tabs() {
        for s in t.subs { for r in s.rows {
            assert!(!r.label.is_empty() && !r.key.is_empty(), "{} {:?}", t.id, r.key);
            match r.bind {
                Bind::Setting(id) => { assert!(settings::entry(id).is_some(), "{id}"); }
                Bind::MouseSens => assert!(settings::entry("input.mouse_sensitivity_y").is_some()),
                _ => {}
            }
        } }
    }
    assert_eq!(hud_tags().len(), 7);
    assert_eq!(hud_tags()[0], "HUD.Center.Reticle");
    assert_eq!(hud_tags(), crate::screens::hud::HudVisibility::TAGS, "the HUD toggles use the Settings screen's tags in its order");
    assert!(!hud_tags().contains(&"HUD.TopLeft.NetDebugUI"), "1.8 has no Net Debug Stats row");
}
#[test]
fn row_keys_are_unique_per_sub_tab() {
    for t in rows::generic_tabs() { for s in t.subs {
        let mut k: Vec<&str> = s.rows.iter().map(|r| r.key).collect(); k.sort(); let n = k.len(); k.dedup();
        assert_eq!(k.len(), n, "{} duplicate row key", t.id);
    } }
}
#[test]
fn rows_carry_no_markers_or_notes_and_gates_only_disable() {
    // the original shows no NOT IMPLEMENTED / RESTART / EXTENSION markers and no "not available" lines: row_info only says enabled or not
    let s = Settings::default();
    let find = |key: &str| layout_table::TABS.iter().flat_map(|t| t.subs.iter()).flat_map(|s| s.rows.iter()).find(|r| r.key == key).unwrap();
    for t in rows::generic_tabs() { for sub in t.subs { for r in sub.rows {
        let info = row_info(r, &s);
        if matches!(r.bind, Bind::Setting(_) | Bind::MouseSens | Bind::Hud { .. } | Bind::Choice { .. }) { assert!(info.enabled, "{} must be a working control", r.key); }
    } } }
    assert!(row_info(find("VSync"), &s).enabled && row_info(find("Language"), &s).enabled && row_info(find("Effects"), &s).enabled && row_info(find("3DResolution"), &s).enabled);
    assert!(!row_info(find("SafeZone"), &s).enabled, "Xbox Safe Zone has no counterpart (only drawn in an Xbox context)");
    assert!(row_info(find("DisplayResolution"), &s).enabled);
    let mut wf = Settings::default(); wf.set("video.window_mode", Value::Index(1)).unwrap();
    assert!(!row_info(find("DisplayResolution"), &wf).enabled, "resolution is inert in Windowed Fullscreen");
}
#[test]
fn matchmaking_region_is_a_real_rotator_over_the_mmregion_strings() {
    let find = |key: &str| layout_table::TABS.iter().flat_map(|t| t.subs.iter()).flat_map(|s| s.rows.iter()).find(|r| r.key == key).unwrap();
    let Bind::Choice { section, key, opts, default } = find("Region").bind else { panic!("Region must be a Choice row") };
    assert_eq!(opts, ["Europe", "North America", "Oceania"], "the MMRegion options of menu-strings.json");
    assert!(default < opts.len());
    let mut s = Settings::default();
    assert_eq!(rows::choice_index(&s, section, key, opts, default), default);
    reset_row(&mut s, find("Region"));
    assert_eq!(s.extra(section, key), Some(opts[default]));
    s.set_extra(section, key, "Oceania");
    assert_eq!(rows::choice_index(&s, section, key, opts, default), 2);
    s.set_extra(section, key, "nonsense");
    assert_eq!(rows::choice_index(&s, section, key, opts, default), default, "an unknown stored value falls back to the default");
    assert!(rows::extra_keys().contains(&(section, key)), "the bridge syncs the choice like the HUD tags");
    let mut other = Settings::default(); other.set_extra(section, key, "Europe");
    assert!(bridge::differences(&Settings::default(), &other).contains(&key.to_string()));
}
// ---- bridge ----------------------------------------------------------------------------------------------------------
#[test]
fn edits_touch_only_the_working_copy_until_apply() {
    let mut mock = Mock::new(); let mut sx = Session::new();
    sx.sync_from(&mock.s);
    assert!(!sx.is_dirty());
    sx.with_settings(|s| { s.set("video.show_fps", Value::Bool(true)).unwrap(); s.set("audio.music_volume", Value::Float(0.2)).unwrap(); });
    assert!(sx.is_dirty());
    let mut ids = sx.changed_ids(); ids.sort();
    assert_eq!(ids, ["audio.music_volume", "video.show_fps"]);
    assert_eq!(mock.s.get("video.show_fps"), Some(Value::Bool(false)), "game untouched before Apply");
    assert_eq!(sx.applied_clone().get("video.show_fps"), Some(Value::Bool(false)));
    sx.request_apply();
    assert!(sx.busy());
    sx.pump(&mut mock.cx());
    assert_eq!(mock.s.get("video.show_fps"), Some(Value::Bool(true)));
    assert_eq!(mock.s.get("audio.music_volume"), Some(Value::Float(0.2)));
    assert!(!sx.is_dirty() && !sx.busy());
    assert_eq!(mock.be.applied_calls, [true]);
    assert_eq!(mock.be.files.len(), 1, "saved once");
    let st = sx.status().unwrap();
    assert_eq!(st.kind, StatusKind::Ok);
    assert!(st.text.contains("Applied and saved") && st.text.contains("stored only"), "{}", st.text);
}
#[test]
fn apply_with_no_changes_is_a_noop_and_discard_restores() {
    let mut mock = Mock::new(); let mut sx = Session::new();
    sx.sync_from(&mock.s);
    sx.request_apply(); assert!(!sx.busy());
    sx.with_settings(|s| { s.set("input.invert_y", Value::Bool(true)).unwrap(); });
    sx.mark_dirty();
    sx.discard_changes();
    assert!(!sx.is_dirty());
    assert_eq!(sx.settings_clone().get("input.invert_y"), Some(Value::Bool(false)));
    sx.pump(&mut mock.cx());
    assert!(mock.be.applied_calls.is_empty());
}
#[test]
fn sync_from_drops_stale_edits() {
    let mut a = Mock::new(); let mut sx = Session::new();
    sx.with_settings(|s| { s.set("video.show_fps", Value::Bool(true)).unwrap(); });
    a.s.set("video.show_fps", Value::Bool(false)).unwrap();
    sx.sync_from(&a.s);
    assert!(!sx.is_dirty());
    assert_eq!(sx.settings_clone().get("video.show_fps"), Some(Value::Bool(false)));
}
#[test]
fn hud_visibility_is_stored_in_the_settings_and_survives_a_save_round_trip() {
    let mut mock = Mock::new(); let mut sx = Session::new();
    sx.sync_from(&mock.s);
    sx.with_settings(|s| rows::hud_set(s, "HUD.Center.Reticle", false));
    assert!(sx.is_dirty());
    assert_eq!(sx.changed_ids(), vec!["HUD.Center.Reticle".to_string()]);
    sx.request_apply(); sx.pump(&mut mock.cx());
    let ini = mock.be.files.last().unwrap().to_ini();
    assert!(ini.contains("HUD.Center.Reticle=False"), "{ini}");
    let (back, _) = Settings::from_ini_bytes(ini.as_bytes()).unwrap();
    assert_eq!(back.extra("HUD", "HUD.Center.Reticle"), Some("False"));
}
#[test]
fn risky_display_change_asks_to_keep_and_keeps_the_old_file_until_confirmed() {
    let mut mock = Mock::new(); let mut sx = Session::new();
    sx.sync_from(&mock.s);
    let old_mode = mock.s.get("video.window_mode").unwrap();
    sx.with_settings(|s| { s.set("video.window_mode", Value::Index(1)).unwrap(); });
    sx.request_apply(); sx.pump(&mut mock.cx());
    let screens = sx.take_screens();
    assert_eq!(screens.len(), 1, "keep-settings dialog queued");
    assert_eq!(screens[0].name(), "settings-modal");
    assert_eq!(mock.s.get("video.window_mode"), Some(Value::Index(1)), "applied live");
    // the file written LAST must still hold the old mode (a crash during the countdown must restore it)
    let last_file = mock.be.files.last().unwrap();
    assert_eq!(last_file.get("video.window_mode"), Some(old_mode), "settings file keeps the previous mode until confirmed");
    assert_eq!(sx.status().unwrap().kind, StatusKind::Info);
    // Yes -> Save
    sx.confirm_keep(); sx.pump(&mut mock.cx());
    assert_eq!(mock.be.files.last().unwrap().get("video.window_mode"), Some(Value::Index(1)));
    assert_eq!(sx.status().unwrap().kind, StatusKind::Ok);
}
#[test]
fn revert_restores_the_previous_display_and_saves() {
    let mut mock = Mock::new(); let mut sx = Session::new();
    sx.sync_from(&mock.s);
    sx.with_settings(|s| { s.set("video.window_mode", Value::Index(1)).unwrap(); s.set("audio.music_volume", Value::Float(0.1)).unwrap(); });
    sx.request_apply(); sx.pump(&mut mock.cx()); let _ = sx.take_screens();
    sx.revert_values(&[("video.window_mode", Value::Index(2))]);
    sx.pump(&mut mock.cx());
    assert_eq!(mock.s.get("video.window_mode"), Some(Value::Index(2)));
    assert_eq!(mock.s.get("audio.music_volume"), Some(Value::Float(0.1)), "non-video edits of the same Apply stay");
    assert_eq!(mock.be.files.last().unwrap().get("video.window_mode"), Some(Value::Index(2)));
    assert!(sx.take_screens().is_empty(), "a revert never asks again");
    assert_eq!(sx.status().unwrap().kind, StatusKind::Warn);
}
#[test]
fn refused_display_mode_is_restored_and_reported() {
    let mut mock = Mock::new(); let mut sx = Session::new();
    mock.be.fail_display = true;
    sx.sync_from(&mock.s);
    sx.with_settings(|s| { s.set("video.window_mode", Value::Index(0)).unwrap(); s.set("video.show_fps", Value::Bool(true)).unwrap(); });
    sx.request_apply(); sx.pump(&mut mock.cx());
    assert_eq!(mock.s.get("video.window_mode"), Some(Value::Index(2)), "refused mode is not kept");
    assert_eq!(sx.settings_clone().get("video.window_mode"), Some(Value::Index(2)));
    assert_eq!(mock.s.get("video.show_fps"), Some(Value::Bool(true)), "other settings of the batch still apply");
    assert!(sx.take_screens().is_empty(), "no keep dialog for a refused mode");
    let st = sx.status().unwrap();
    assert_eq!(st.kind, StatusKind::Error);
    assert!(st.text.contains("refused"), "{}", st.text);
}
#[test]
fn preview_applies_without_saving_and_discard_rolls_it_back() {
    let mut mock = Mock::new(); let mut sx = Session::new();
    sx.sync_from(&mock.s);
    sx.with_settings(|s| { s.set("video.brightness", Value::Float(0.9)).unwrap(); });
    sx.request_preview(); sx.pump(&mut mock.cx());
    assert_eq!(mock.s.get("video.brightness"), Some(Value::Float(0.9)));
    assert_eq!(mock.be.applied_calls, [false], "preview must not save");
    assert!(mock.be.files.is_empty());
    assert!(sx.is_dirty(), "a preview is not saved yet, so Apply must stay available");
    sx.discard_changes(); sx.pump(&mut mock.cx());
    assert_eq!(mock.s.get("video.brightness"), Some(Value::Float(0.5)), "rolled back to the saved value");
    assert!(!sx.is_dirty());
    // preview then Apply saves
    sx.with_settings(|s| { s.set("video.brightness", Value::Float(0.7)).unwrap(); });
    sx.request_preview(); sx.pump(&mut mock.cx());
    sx.request_apply(); sx.pump(&mut mock.cx());
    assert_eq!(mock.be.files.last().unwrap().get("video.brightness"), Some(Value::Float(0.7)));
}
#[test]
fn standalone_mode_saves_the_file_without_a_game() {
    let dir = std::env::temp_dir().join(format!("fnrt-pages-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("FortniteSettings.ini");
    let mut sx = Session::new();
    sx.attach_standalone(Some(path.clone()));
    assert!(!sx.is_attached());
    sx.with_settings(|s| { s.set("audio.sfx_volume", Value::Float(0.25)).unwrap(); });
    sx.request_apply();
    assert!(!sx.is_dirty() && !sx.busy());
    let (loaded, rep) = Settings::load(&path);
    assert_eq!(rep.status, settings::LoadStatus::Loaded);
    assert_eq!(loaded.get("audio.sfx_volume"), Some(Value::Float(0.25)));
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn copying_working_values_preserves_preset_consistency() {
    // every combination of quality edits must leave the live settings equal to the working copy
    let mut mock = Mock::new(); let mut sx = Session::new();
    for (id, v) in [("video.quality", 0usize), ("video.shadows", 2), ("video.quality", 3), ("video.view_distance", 1), ("video.post_processing", 0)] {
        sx.sync_from(&mock.s);
        sx.with_settings(|s| { s.set(id, Value::Index(v)).unwrap(); });
        let w = sx.settings_clone();
        sx.request_apply(); sx.pump(&mut mock.cx());
        assert!(w.same_values(&mock.s), "{id}={v}: {:?}", w.diff(&mock.s));
    }
}

// ---- screens ----------------------------------------------------------------------------------------------------------
#[test]
fn all_seven_tabs_exist_in_original_order() {
    let h = H::new(1920., 1080.);
    let t: Vec<&str> = h.m.pages().iter().map(|p| p.title()).collect();
    assert_eq!(t, ["Video", "Game", "Brightness", "Audio", "Input", "Controller", "Live Streaming"]);
}
#[test]
fn specialised_page_replaces_the_generic_one_in_either_order() {
    struct P(&'static str);
    impl SettingsPage for P { fn id(&self) -> &str { self.0 } fn title(&self) -> &str { "Special" } fn order(&self) -> i32 { ORDER_VIDEO } fn ui(&mut self, _: &mut Ui, _: Rect) {} }
    // special first, generic after
    let mut m = MenuSystem::new(fonts());
    m.register_settings_page(Box::new(P("video")));
    register_generic_into(&mut m);
    m.open();
    let titles: Vec<String> = m.pages().iter().map(|p| p.title().to_owned()).collect();
    assert_eq!(titles.iter().filter(|t| *t == "Special").count(), 1);
    assert_eq!(titles.iter().filter(|t| *t == "Video").count(), 0, "generic Video must not appear");
    assert_eq!(titles.len(), 7);
    // generic first, special after
    let mut m2 = MenuSystem::new(fonts());
    for t in rows::generic_tabs() { m2.register_settings_page(Box::new(GenericPage::new(t))); }
    m2.register_settings_page(Box::new(P("video")));
    assert_eq!(m2.pages().len(), 7);
    assert_eq!(m2.pages()[0].title(), "Special");
    // two non-fallback pages: last wins, no duplicates
    m2.register_settings_page(Box::new(P("video")));
    assert_eq!(m2.pages().len(), 7);
}
#[test]
fn video_tab_lists_rows_and_labels_without_markers() {
    let mut h = H::new(1920., 1080.);
    h.tab(0);
    let t = h.texts();
    for l in ["Window Mode", "Display Resolution", "Frame Rate Limit", "Quality", "3D Resolution", "View Distance", "Shadows", "Anti-Aliasing", "Textures", "Effects", "Post Processing", "Vsync"] {
        assert!(t.iter().any(|x| x == l), "missing {l}: {t:?}");
    }
    for bad in ["NOT IMPLEMENTED", "RESTART", "EXTENSION", "NOT USED", "OFFLINE", "Not available in this build."] { assert!(!t.iter().any(|x| x.contains(bad)), "{bad:?} is not original text: {t:?}"); }
    // 1.8 footer (bytecode SetActionHandlerState): Video shows Restore + Back; Apply only appears once something changed; no Reset To Default
    let u: Vec<&String> = t.iter().filter(|x| *x == "Apply" || *x == "Reset To Default" || *x == "Back" || *x == "Restore").collect();
    assert_eq!(u, ["Restore", "Back"], "footer buttons while clean");
    h.m.session_mut().with_settings(|s| { s.set("video.show_fps", Value::Bool(true)).unwrap(); });
    let u: Vec<String> = h.texts().into_iter().filter(|x| x == "Apply" || x == "Reset To Default" || x == "Back" || x == "Restore").collect();
    assert_eq!(u, ["Apply", "Restore", "Back"], "Apply appears when dirty");
}
#[test]
fn game_tab_shows_the_17_pc_battle_royale_rows_and_hud_sub_tab_has_7() {
    let mut h = H::new(1920., 1080.);
    let g = h.page_index("game");
    h.tab(g);
    // the 1.8 original has 21 Game rows; on PC in Battle Royale the bytecode collapses Xbox Safe Zone,
    // First-Person Camera, Show Head Accessories and Show Backpack
    assert_eq!(layout_table::TABS[1].subs[0].rows.iter().filter(|r| !r.extension).count(), 21);
    let ctx = visibility::MenuContext::default();
    let labels: Vec<&str> = visibility::visible_rows("game", &layout_table::TABS[1].subs[0], &ctx).iter().map(|r| r.label).collect();
    assert_eq!(labels.len(), 17);
    for gone in ["Confirm Edit on Release", "Reset Edit on Release", "Turbo Building"] { assert!(!labels.contains(&gone), "{gone}"); }
    for gone in ["Xbox Safe Zone", "First-Person Camera", "Show Head Accessories", "Show Backpack"] { assert!(!labels.contains(&gone), "{gone}"); }
    // the list scrolls: all rows must be reachable, so scroll down in steps and collect what was drawn
    let mut seen = std::collections::HashSet::new();
    for _ in 0..12 {
        for t in h.texts() { seen.insert(t); }
        h.m.event(Event::MouseMove(960., 400.)); h.m.event(Event::Wheel(-3.)); h.frames(2);
    }
    for l in &labels { assert!(seen.contains(*l), "Game row {l:?} never drawn"); }
    for gone in ["Xbox Safe Zone", "Uncap Framerate", "First-Person Camera", "Show Head Accessories", "Show Backpack"] { assert!(!seen.contains(gone), "{gone} must not be drawn on PC / Battle Royale"); }
    // HUD sub tab via keyboard: focus the sub tab bar is complex; click it instead
    let (x, y) = h.pos_of("HUD");
    h.click(x, y);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..4 { for t in h.texts() { seen.insert(t); } h.m.event(Event::Wheel(-3.)); h.frames(2); }
    for l in ["Reticle", "Player Health", "Pickup Info", "Minimap", "Quick Bar", "Target Info", "Interaction Info"] { assert!(seen.contains(l), "HUD row {l}"); }
    assert!(!seen.contains("Net Debug Stats"), "1.8 HUD has no Net Debug Stats row");
}
#[test]
fn live_streaming_tab_is_the_original_text_only() {
    let mut h = H::new(1920., 1080.);
    let i = h.page_index("livestream");
    h.tab(i);
    let t = h.texts();
    assert!(t.iter().any(|x| x.starts_with("Link your Twitch account")) || t.iter().any(|x| x.starts_with("Link")), "{t:?}");
    assert!(!t.iter().any(|x| x.contains("Not available")), "no invented note: {t:?}");
    assert!(!t.iter().any(|x| x.contains("NOT IMPLEMENTED") || x.contains("runtime")), "no developer text: {t:?}");
}
#[test]
fn keyboard_edit_marks_dirty_and_apply_button_applies() {
    let mut h = H::new(1920., 1080.);
    let g = h.page_index("video"); h.tab(g);
    // focus: tab bar (first stop) then the first row... Down moves through the order: bar, sub?, rows
    h.key(NavKey::Down); h.key(NavKey::Down); // window mode row (after the tab bar and close/back stops may vary)
    let before = h.m.session_mut().settings_clone();
    let mut changed = false;
    for _ in 0..8 { h.key(NavKey::Right); if !h.m.session_mut().changed_ids().is_empty() { changed = true; break; } h.key(NavKey::Down); }
    assert!(changed, "some row reacted to Right");
    assert!(h.m.session_mut().is_dirty());
    assert!(!h.m.session_mut().settings_clone().same_values(&before));
    let (x, y) = h.pos_of("Apply"); // the Apply footer button exists only while dirty (1.8)
    h.click(x, y);
    h.frames(2);
    assert!(!h.game.be.applied_calls.is_empty(), "Apply reached the game");
}
#[test]
fn back_with_unapplied_changes_discards_and_leaves_without_a_prompt() {
    let mut h = H::new(1920., 1080.);
    h.m.session_mut().with_settings(|s| { s.set("video.show_fps", Value::Bool(true)).unwrap(); });
    assert!(h.m.session_mut().is_dirty());
    // 1.8 OptionsMenu: Back pops the screen and the unapplied values are dropped; there is no unsaved-changes dialog
    h.key(NavKey::Back);
    assert!(!h.stack().iter().any(|s| s == "settings" || s == "settings-modal"), "left settings without a prompt: {:?}", h.stack());
    assert!(!h.m.session_mut().is_dirty(), "Back dropped the edits");
}
#[test]
fn back_without_changes_goes_back_one_level() {
    let mut h = H::new(1024., 768.);
    assert_eq!(h.stack(), ["pause", "settings"]);
    h.key(NavKey::Back);
    assert_eq!(h.stack(), ["pause"]);
}
#[test]
fn restore_discards_edits_on_game_video_and_reset_to_default_lives_on_the_input_tab() {
    // 1.8 footer: Video/Game show Restore (R) and no Reset To Default; no confirmation dialog exists for either.
    let mut h = H::new(1920., 1080.);
    let g = h.page_index("game"); h.tab(g);
    h.m.session_mut().with_settings(|s| { s.set("input.invert_y", Value::Bool(true)).unwrap(); });
    assert!(!h.has("Reset To Default"), "Game tab has no Reset To Default in 1.8");
    let (x, y) = h.pos_of("Restore");
    h.click(x, y);
    assert_eq!(h.stack().last().map(String::as_str), Some("settings"), "Restore asks nothing: {:?}", h.stack());
    assert_eq!(h.m.session_mut().settings_clone().get("input.invert_y"), Some(Value::Bool(false)), "Restore drops the working copy edits");
    assert!(!h.m.session_mut().is_dirty());
    assert!(h.game.be.applied_calls.is_empty(), "Restore applies nothing");
    // Video tab: same
    h.tab(0);
    h.m.session_mut().with_settings(|s| { s.set("video.show_fps", Value::Bool(true)).unwrap(); });
    assert!(!h.has("Reset To Default"));
    let (x, y) = h.pos_of("Restore");
    h.click(x, y);
    assert_eq!(h.m.session_mut().settings_clone().get("video.show_fps"), Some(Value::Bool(false)));
    // Input tab (keyboard): Reset To Default is shown, Restore is not
    let i = h.page_index("input"); h.tab(i);
    assert!(h.has("Reset To Default") && !h.has("Restore"));
}
#[test]
fn tab_switch_mid_edit_keeps_edits_and_dropdown_state_is_clean() {
    let mut h = H::new(1920., 1080.);
    h.tab(0);
    h.m.session_mut().with_settings(|s| { s.set("video.show_fps", Value::Bool(true)).unwrap(); });
    for i in [3usize, 1, 0, 6, 2] { h.tab(i); assert!(h.m.session_mut().is_dirty()); }
    assert_eq!(h.m.session_mut().settings_clone().get("video.show_fps"), Some(Value::Bool(true)));
    h.key(NavKey::TabNext); h.key(NavKey::TabPrev);
    assert!(h.m.session_mut().is_dirty());
}
#[test]
fn resize_while_open_keeps_everything_working() {
    let mut h = H::new(1920., 1080.);
    h.tab(0);
    h.m.session_mut().with_settings(|s| { s.set("video.show_fps", Value::Bool(true)).unwrap(); });
    for (w, hh) in SIZES.iter().chain([(1., 1.), (200., 100.), (4000., 300.)].iter()) {
        h.w = *w; h.h = *hh; h.frames(3);
        assert!(h.m.session_mut().is_dirty());
    }
    h.w = 1280.; h.h = 720.; h.frames(3);
    assert!(h.has("Window Mode"));
}
#[test]
fn every_tab_draws_inside_the_client_at_all_sizes() {
    // The 1.8 OptionsMenu is a fixed Slate layout (no reflow): it needs ~1280 Slate units of width. Narrower viewports (portrait windows)
    // overflow in the original too; they are only required not to panic.
    for (w, hh) in SIZES {
        let vp = crate::layout::Viewport::new(w, hh);
        let fits = vp.slate_size().0 >= 1280.;
        let mut h = H::new(w, hh);
        for i in 0..h.m.pages().len() {
            h.tab(i);
            for c in h.frame() {
                if let Cmd::Text { x, top, box_h, text, .. } = &c {
                    if fits { assert!(*x >= -1. && *x <= w + 1., "{w}x{hh} tab {i}: text {text:?} x {x}"); }
                    let _ = (top, box_h);
                }
            }
        }
    }
}
#[test]
fn matchmaking_region_row_is_an_ordinary_rotator_without_notes() {
    let mut h = H::new(1920., 1080.);
    let g = h.page_index("game"); h.tab(g);
    for _ in 0..12 { if h.texts().iter().any(|t| t == "Matchmaking Region") { break; } h.m.event(Event::MouseMove(960., 400.)); h.m.event(Event::Wheel(-3.)); h.frames(2); }
    let (x, y) = h.pos_of("Matchmaking Region");
    h.mv(x + 20., y);
    let t = h.texts();
    assert!(t.iter().any(|t| t == "North America" || t == "Europe" || t == "Oceania"), "the rotator shows one MMRegion option: {t:?}");
    assert!(!t.iter().any(|t| t.contains("Not available") || t == "n/a"), "no placeholder text: {t:?}");
    assert!(t.iter().any(|t| t == "Change the matchmaking region to use."), "original hover text: {t:?}");
}#[test]
fn a_dragged_slider_keeps_tracking_outside_the_widget_and_clamps() {
    let mut h = H::new(1920., 1080.);
    let a = h.page_index("audio"); h.tab(a);
    let (x, y) = h.pos_of("Music Volume");
    // slider thumb: press on the track right of the label, drag far outside the window, release outside
    let (tx, ty) = (x + 500., y); // inside the 1.8 slider track (label column 300 + gap 20, control >= 400)
    h.mv(tx, ty); h.m.event(Event::MouseDown(MouseButton::Left, tx, ty)); h.frame();
    h.mv(5000., ty + 400.); h.frame();
    assert_eq!(h.m.session_mut().settings_clone().get("audio.music_volume"), Some(Value::Float(1.0)), "dragged beyond the right end = max");
    h.mv(-5000., ty); h.frame();
    assert_eq!(h.m.session_mut().settings_clone().get("audio.music_volume"), Some(Value::Float(0.0)), "dragged beyond the left end = min");
    h.m.event(Event::MouseUp(MouseButton::Left, -5000., ty)); h.frames(2);
    assert!(h.m.session_mut().is_dirty());
}
#[test]
fn modal_layout_stays_inside_the_viewport() {
    use super::dialogs::modal_layout;
    for (w, hh) in SIZES {
        let vp = crate::layout::Viewport::new(w, hh);
        let (vw, vh) = vp.slate_size();
        for n in 1..=3 {
            let (panel, body, btns) = modal_layout(vw, vh, vp.scale, 60., n);
            assert!(panel.x >= -0.5 && panel.x + panel.w <= w + 0.5, "{w}x{hh} panel x");
            for b in btns { assert!(b.x >= panel.x - 0.5 && b.x + b.w <= panel.x + panel.w + 0.5, "{w}x{hh} button inside panel"); }
            assert!(body.w > 0.);
        }
    }
}
#[test]
fn keep_video_dialog_counts_down_and_reverts_on_timeout() {
    let mut h = H::new(1920., 1080.);
    h.m.session_mut().with_settings(|s| { s.set("video.window_mode", Value::Index(1)).unwrap(); });
    h.m.session_mut().request_apply(); h.frames(3);
    assert_eq!(h.stack().last().map(String::as_str), Some("settings-modal"), "{:?}", h.stack());
    assert!(h.texts().iter().any(|t| t == "Do you want to keep these video settings?"));
    assert!(h.texts().iter().any(|t| t.starts_with("Reverting in 15") || t.starts_with("Reverting in 14")), "{:?}", h.texts());
    for _ in 0..(16 * 60) { h.frame(); }
    assert_eq!(h.game.s.get("video.window_mode"), Some(Value::Index(2)), "timed out: reverted");
    assert_eq!(h.stack().last().map(String::as_str), Some("settings"), "dialog closed: {:?}", h.stack());
}
#[test]
fn keep_video_dialog_yes_keeps_and_esc_reverts() {
    let mut h = H::new(1920., 1080.);
    h.m.session_mut().with_settings(|s| { s.set("video.window_mode", Value::Index(1)).unwrap(); });
    h.m.session_mut().request_apply(); h.frames(3);
    h.key(NavKey::Left); h.key(NavKey::Activate); // Yes
    h.frames(3);
    assert_eq!(h.game.s.get("video.window_mode"), Some(Value::Index(1)));
    assert_eq!(h.game.be.files.last().unwrap().get("video.window_mode"), Some(Value::Index(1)));
    // second change, Esc = No
    h.m.session_mut().with_settings(|s| { s.set("video.window_mode", Value::Index(0)).unwrap(); });
    h.m.session_mut().request_apply(); h.frames(3);
    h.key(NavKey::Back); h.frames(3);
    assert_eq!(h.game.s.get("video.window_mode"), Some(Value::Index(1)), "Esc reverted to the kept mode");
}
#[test]
fn closing_the_menu_during_the_countdown_reverts() {
    let mut h = H::new(1920., 1080.);
    h.m.session_mut().with_settings(|s| { s.set("video.resolution", Value::Size(1600, 900)).unwrap(); });
    h.m.session_mut().request_apply(); h.frames(3);
    assert_eq!(h.stack().last().map(String::as_str), Some("settings-modal"));
    let before = h.game.s.get("video.resolution");
    assert_eq!(before, Some(Value::Size(1600, 900)));
    h.m.close(); h.frames(2);
    assert_ne!(h.game.s.get("video.resolution"), Some(Value::Size(1600, 900)), "menu closed without an answer = revert");
}
#[test]
fn footer_layout_never_overlaps_at_any_size() {
    use super::screen18::{footer_actions, footer_rects};
    let f = fonts();
    // landscape only: `UIScaleRule=Vertical` (CookedIniVersion.txt L531) scales by the height, so a portrait client is narrower in Slate units than the 1.8 footer
    for (w, hh) in SIZES.into_iter().filter(|(w, hh)| w >= hh) {
        let vp = crate::layout::Viewport::new(w, hh);
        let (vw, vh) = vp.slate_size();
        for tab in ["video", "game", "input", "controller", "livestream", "audio"] {
            let acts = footer_actions(tab, true, true, false, &mut [true, false]);
            let r = footer_rects(&f, vp.scale, vw, vh, &acts);
            for (i, (b, _)) in r.iter().enumerate() {
                assert!(b.x >= -0.5 && b.x + b.w <= vw + 0.5, "{w}x{hh} {tab}: button {i} inside the bar");
                if i > 0 { assert!(r[i - 1].0.x + r[i - 1].0.w <= b.x + 0.5, "{w}x{hh} {tab}: buttons {} and {i} overlap", i - 1); }
            }
        }
    }
}
#[test]
fn text_colors_are_exported_for_markers() { assert!(TEXT_DIM[0] > 0.); }


// ---- UI polish (QA round): description fitting, tab strip, small windows, honest apply text -----------------------------------
fn longest_strings(n: usize) -> Vec<String> {
    let j: serde_json::Value = crate::testutil::report_json("menu-strings.json");
    let mut v: Vec<String> = j["strings"].as_array().unwrap().iter().filter_map(|s| s["en"].as_str().map(str::to_owned)).collect();
    v.extend(rows::generic_tabs().iter().flat_map(|t| t.subs.iter()).flat_map(|s| s.rows.iter()).flat_map(|r| r.tips.iter().map(|t| t.to_string())));
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v.truncate(n); v
}
#[test]
fn description_panel_never_cuts_a_line_for_the_longest_hover_strings() {
    let f = fonts();
    let strings = longest_strings(16);
    assert!(strings[0].len() > 150, "the longest hover string is long: {}", strings[0].len());
    let extra = "A second paragraph of tooltip text that is long enough to wrap onto several lines in a narrow panel.";
    let (mut fitted, mut truncated) = (0, 0);
    for scale in [0.444f32, 0.666, 1.0, 1.5, 2.0] {
        for (w, h) in [(120., 30.), (250., 40.), (300., 60.), (500., 90.), (900., 120.), (1200., 200.), (1200., 90.)] {
            for s in &strings {
                let blocks = vec![
                    rows::DescBlock::new("Label", FontId::Bold, 17., TEAL_BRIGHT, 4.),
                    rows::DescBlock::new(s, FontId::Regular, 15., TEXT, 4.),
                    rows::DescBlock::new(extra, FontId::Regular, 14., TEXT_DIM, 2.),
                ];
                let lines = rows::fit_desc_blocks(&f, &blocks, scale, w, h);
                assert!(!lines.is_empty());
                for l in &lines {
                    assert!(l.y >= 0. && l.y + l.h <= h + 0.5, "scale {scale} {w}x{h}: line {:?} y {} h {} leaves the panel", l.text, l.y, l.h);
                    assert!(f.measure(l.font, &l.text, l.px as f32) <= w + 0.5, "scale {scale} {w}x{h}: line {:?} wider than the panel", l.text);
                }
                let cut = lines.last().map_or(false, |l| l.text.ends_with("..."));
                if cut { truncated += 1; } else {
                    fitted += 1;
                    let words: usize = lines.iter().map(|l| l.text.split_whitespace().count()).sum();
                    let want = sanitize("Label").split_whitespace().count() + sanitize(s).split_whitespace().count() + sanitize(extra).split_whitespace().count();
                    assert_eq!(words, want, "scale {scale} {w}x{h}: an untruncated panel shows every word of {s:?}");
                }
            }
        }
    }
    assert!(fitted > 100 && truncated > 0, "both outcomes are exercised: {fitted} fitted, {truncated} ellipsised");
}
#[test]
fn description_panel_shrinks_before_it_drops_lines() {
    let f = fonts();
    let s = longest_strings(1).remove(0);
    let blocks = vec![rows::DescBlock::new("Label", FontId::Bold, 17., TEAL_BRIGHT, 4.), rows::DescBlock::new(&s, FontId::Regular, 15., TEXT, 4.)];
    let full = rows::fit_desc_blocks(&f, &blocks, 1., 400., 10_000.);
    let total: f32 = full.last().map(|l| l.y + l.h).unwrap();
    let squeezed = rows::fit_desc_blocks(&f, &blocks, 1., 400., total * 0.8);
    assert!(squeezed.iter().all(|l| !l.text.ends_with("...")), "80% of the height still fits entirely by shrinking the font");
    assert!(squeezed[1].px < full[1].px, "font shrank: {} < {}", squeezed[1].px, full[1].px);
}
#[test]
fn tab_layout_degrades_without_truncating_text() {
    use crate::menu::widgets::{tab_layout, TabLayout};
    let (natural, compact) = ([100., 100., 140., 100., 100., 120., 160.], [60., 60., 90., 60., 60., 80., 100.]);
    let (sn, sc): (f32, f32) = (natural.iter().sum(), compact.iter().sum());
    assert_eq!(tab_layout(&natural, &compact, 0, sn, 18.), TabLayout::Fit(natural.to_vec()));
    // between the two sums: widths never fall below the text-sized width and fill the bar
    let TabLayout::Fit(w) = tab_layout(&natural, &compact, 0, (sn + sc) * 0.5, 18.) else { panic!("fit") };
    assert!(w.iter().zip(&compact).all(|(a, c)| a >= c) && (w.iter().sum::<f32>() - (sn + sc) * 0.5).abs() < 0.01);
    // narrower than the compact row: a window of whole tabs that always holds the selected one
    for sel in 0..7 {
        for avail in [60., 100., 150., 220., 300., sc - 1.] {
            let TabLayout::Window { first, last, widths } = tab_layout(&natural, &compact, sel, avail, 18.) else { panic!("window at {avail}") };
            assert!(first <= sel && sel < last && last <= 7 && widths.len() == last - first, "sel {sel} avail {avail}: {first}..{last}");
            assert!(widths.iter().sum::<f32>() <= (avail - 36.).max(0.) + 0.01 || widths.len() == 1);
            if widths.len() > 1 { assert!(widths.iter().zip(&compact[first..last]).all(|(a, c)| (a - c).abs() < 1e-4), "whole tabs, no shrinking"); }
        }
    }
}
#[test]
fn settings_tabs_are_complete_at_the_minimum_window_and_stay_sane_below_it() {
    let f = fonts();
    let titles = ["Video", "Game", "Brightness", "Audio", "Input", "Controller", "Live Streaming"];
    for (w, h) in [(640., 360.), (800., 450.), (1024., 768.), (640., 480.), (800., 1280.), (1920., 1080.), (2560., 1080.)] {
        let mut s = H::new(w, h);
        let t = s.texts();
        for title in titles { assert!(t.iter().any(|x| x == title), "{w}x{h}: tab {title:?} missing or truncated in {t:?}"); }
    }
    // below the minimum: no panic, no text outside the client, never the settings screen overlapped by the notice
    for (w, h) in [(184., 81.), (256., 144.), (320., 180.), (400., 300.), (480., 270.), (300., 700.), (120., 400.), (600., 340.), (400., 225.), (360., 640.)] {
        let mut s = H::new(w, h);
        for tab in 0..7 {
            s.tab(tab);
            let cmds = s.frame();
            let fits = crate::layout::Viewport::new(w, h).slate_size().0 >= 1280.; // the fixed 1.8 layout does not reflow below this
            // clip-aware: text that a scroll list / panel clips away entirely is fine, but what is VISIBLE must lie inside the client
            let mut clips: Vec<Rect> = Vec::new();
            for c in &cmds { match c {
                Cmd::PushClip(r) => clips.push(*r), Cmd::PopClip => { clips.pop(); }
                Cmd::Text { x, top, box_h, text, font, px, .. } => {
                    let tw = f.measure(*font, text, *px as f32);
                    let tr = Rect::new(*x, *top, tw, *box_h);
                    let vis = match clips.last() { Some(cl) => rect_intersect(tr, *cl), None => Some(tr) };
                    if let Some(v) = vis.filter(|_| fits) { assert!(v.x >= -0.5 && v.x + v.w <= w + 0.5 && v.y >= -0.5 && v.y + v.h <= h + 0.5, "{w}x{h} tab {tab}: visible part of {text:?} {v:?} leaves the client"); }
                }
                _ => {}
            } }
            if too_small(w, h) { assert!(cmds.iter().any(|c| matches!(c, Cmd::Text { text, .. } if text.contains("too small") || text.contains("Window") || text.contains("Esc"))), "{w}x{h}: notice shown"); }
            else { assert!(cmds.iter().any(|c| matches!(c, Cmd::Text { text, .. } if text == "Back")), "{w}x{h}: settings screen drawn"); }
        }
        // wheel / click / back while tiny must not panic and Esc still leaves
        s.m.event(Event::MouseMove(w * 0.5, h * 0.5)); s.m.event(Event::Wheel(-3.)); s.frames(2);
        s.click(w * 0.5, h * 0.5);
        s.key(NavKey::Back); s.key(NavKey::Back); s.frames(2);
    }
}
#[test]
fn save_status_is_honest_when_the_host_writes_nothing() {
    let mut mock = Mock::new(); let mut sx = Session::new(); mock.be.persist = false;
    sx.sync_from(&mock.s);
    sx.with_settings(|s| { let _ = s.set("audio.music_volume", Value::Float(0.3)); });
    sx.request_apply(); sx.pump(&mut mock.cx());
    let st = sx.status().expect("status"); assert!(!st.text.contains("saved") || st.text.contains("not saved"), "{}", st.text);
    assert!(st.text.contains("not saved"), "{}", st.text);
    // the explicit Save request (after the keep-video-settings dialog) says the same
    sx.confirm_keep(); sx.pump(&mut mock.cx());
    let st = sx.status().unwrap(); assert!(st.text.contains("not saved"), "{}", st.text);
    // a persisting host still reports the save
    let mut real = Mock::new(); sx.sync_from(&real.s);
    sx.with_settings(|s| { let _ = s.set("audio.music_volume", Value::Float(0.4)); });
    sx.request_apply(); sx.pump(&mut real.cx());
    assert!(sx.status().unwrap().text.starts_with("Applied and saved"), "{}", sx.status().unwrap().text);
    // the capture host (--settings-screen-test) is a non-persisting host
    assert!(!NullHost::new().backend.persists());
}
