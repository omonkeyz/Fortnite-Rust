//! Tests of the BR lobby UI against the 1.8 install (skipped when the install is not on this machine).
use super::slate_mat::SlateMat;
use super::store::Store;
use super::*;
use crate::material::uimat::{self, Mat, MatParams, MatState};
use crate::scene::UiContext;
use std::cell::RefCell;

fn install() -> Option<std::path::PathBuf> {
    let p = crate::pak_assets::default_install();
    if p.exists() { Some(p) } else { eprintln!("skipping: Fortnite install {} not found (set FN_ROOT)", p.display()); None }
}

thread_local! { static SHARED: RefCell<Option<(AthenaLobbyUi, UiContext)>> = const { RefCell::new(None) }; }

fn with_lobby<R>(f: impl FnOnce(&mut AthenaLobbyUi, &mut UiContext) -> R) -> Option<R> {
    let inst = install()?;
    SHARED.with(|s| {
        let mut s = s.borrow_mut();
        if s.is_none() {
            let uic = UiContext::from_install(&inst, None).expect("ui context");
            let lobby = AthenaLobbyUi::load(&inst).expect("lobby ui");
            *s = Some((lobby, uic));
        }
        let (l, u) = s.as_mut().unwrap();
        Some(f(l, u))
    })
}

fn near(a: f32, b: f32, what: &str) { assert!((a - b).abs() < 0.51, "{what}: {a} vs expected {b}"); }

/// Mean absolute difference (0..255, RGB weighted by alpha) of two straight RGBA8 images.
fn mean_diff(a: &[u8], b: &[u8]) -> f32 {
    let mut s = 0.0f64;
    for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let w = (p[3].max(q[3]) as f64) / 255.0;
        for c in 0..3 { s += (p[c] as f64 - q[c] as f64).abs() * w; }
        s += (p[3] as f64 - q[3] as f64).abs();
    }
    (s / (a.len() as f64 / 4.0 * 4.0)) as f32
}

/// The generic cooked-shader evaluation (`SlateMat`: instance chain + uniform expression set + compiled Slate PS on `fn-dxbc`'s interpreter) draws the
/// same pixels as the generated Settings-screen ports of the very same materials (`material::uimat`).
#[test]
fn generic_material_evaluation_matches_the_generated_ports() {
    let Some(inst) = install() else { return };
    let store = Store::open(&inst).unwrap();
    let mut uic = UiContext::from_install(&inst, None).unwrap();
    let cases: [(&str, Mat, MatState, u32, u32); 5] = [
        ("/Game/UI/Foundation/Buttons/Materials/Tab/M-Tab-Main-Base.M-Tab-Main-Base", Mat::TabMain, MatState::Base, 96, 30),
        ("/Game/UI/Foundation/Buttons/Materials/Tab/M-Tab-Main-Pressed.M-Tab-Main-Pressed", Mat::TabMain, MatState::Pressed, 96, 30),
        ("/Game/UI/Foundation/Border/Materials/M-Border-MainL.M-Border-MainL", Mat::BorderMainL, MatState::Base, 200, 120),
        ("/Game/UI/Foundation/Buttons/Materials/Primary/M-Button-Primary-M-Base.M-Button-Primary-M-Base", Mat::RadioM, MatState::Base, 160, 48),
        ("/Game/UI/Foundation/Buttons/Materials/M-Button-Outline-Base-Left.M-Button-Outline-Base-Left", Mat::ArrowLeft, MatState::Base, 18, 20),
    ];
    for (path, mat, state, w, h) in cases {
        let Some(b) = uimat::default_brush(mat, state) else { panic!("no brush for {path}") };
        let old = uimat::raster(mat, state, w, h, &MatParams::default());
        let m = SlateMat::load(&store, &mut uic.assets, path).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert!(m.is_ready(), "{path}: {:?}", m.warnings);
        let boxed = b.draw == uimat::Draw::Box && b.margin.iter().any(|x| *x != 0.0);
        let new = m.raster(w, h, (w as f32, h as f32), b.native_size, b.margin, boxed, b.tint, false, 0.0);
        let d = mean_diff(&old, &new);
        eprintln!("{path}: mean diff {d:.3} / 255");
        assert!(d < 1.5, "{path}: generic evaluation differs from the generated port by {d}");
    }
}

#[test]
fn the_screen_is_composed_the_way_the_front_end_creates_it() {
    let Some(()) = with_lobby(|l, uic| {
        let mut st = LobbyState::default();
        st.selected_tab = 0;
        let f = l.draw(uic, 1920.0, 1080.0, &st);
        // FrontEnd -> MainContentStack -> AthenaTabsScreen -> tab list + AthenaLobby
        for n in ["FrontEnd", "MainContentStack", "AthenaTabsScreen", "TopTabList", "AthenaLobby", "OverlayMain", "Launch", "PlayButton", "PlaylistRotator", "TeamFillRotator", "PlayerPanel", "TopBar", "Social", "BottomBar", "FortniteLogo", "MainMenuButton"] {
            assert!(f.rect(n).is_some(), "missing widget {n}");
        }
        // tabs: `PreregisteredTabInfoArray` order, hidden registrations (Compendium) not created
        let labels: Vec<&str> = f.tabs.iter().map(|t| t.0.as_str()).collect();
        // AthenaTabsScreen::HandleTabCreated: ButtonText = TextToUpper(ButtonText)
        assert_eq!(labels, ["LOBBY", "LOCKER", "ITEM SHOP", "SEASON SHOP", "STATS", "LEADERBOARDS"]);
        assert!(f.missing.is_empty(), "unresolved assets: {:?}", f.missing);
    }) else { return };
}

/// The rendered rectangles equal the slot math of the data: values below are the serialized slot / size numbers (AthenaTabsScreen, AthenaLobby,
/// AthenaMatchmakingWidget, AthenaRotator, ButtonStyle-Primary-L, FrontEnd spacers), combined by hand the way Slate lays them out.
#[test]
fn key_element_rectangles_equal_the_slot_math() {
    let Some(()) = with_lobby(|l, uic| {
        for (w, h) in [(1920.0f32, 1080.0f32), (1280.0, 720.0)] {
            let s = crate::layout::dpi_scale(w, h);
            let f = l.draw(uic, w, h, &LobbyState::default());
            let (vw, vh) = (w / s, h / s);
            let px = |v: f32| v * s;
            // --- FrontEnd: TopBarSizedSpacer (0, 86) above the content, BottomBarSizedSpacer (0, 60) below it
            let content_top = 86.0;
            let content_bottom = vh - 60.0;
            let tabs_screen = f.rect("AthenaTabsScreen").unwrap();
            near(tabs_screen.y, px(content_top), "AthenaTabsScreen.y"); near(tabs_screen.h, px(content_bottom - content_top), "AthenaTabsScreen.h");
            // --- AthenaTabsScreen: VerticalBox [TopTabList (centre / centre; SizeBox_231.HeightOverride = 88 is IGNORED: bOverride_HeightOverride is not set), switcher (Fill)]
            let list = f.rect("TopTabList").unwrap();
            let list_h = {
                let px_size = crate::screens::atlas_px(16.0, s);
                uic.fonts.line_height(crate::text::FontId::Bold, px_size as f32) / s + 8.0
            };
            near(list.h, px(list_h), "TopTabList.h (= tab height)"); near(list.y, px(content_top), "TopTabList.y");
            near(list.x + list.w * 0.5, w * 0.5, "TopTabList centred horizontally");
            // each tab = text + CenterButtonTextWidget slot padding 4+4 + ButtonStyle-Tab-Main ButtonPadding 20+20; slot padding TabButtonPadding 4+4 between tabs
            let px_size = crate::screens::atlas_px(16.0, s);
            let line_h = uic.fonts.line_height(crate::text::FontId::Bold, px_size as f32) / s;
            // TabButtonPadding of the TopTabList instance in AthenaTabsScreen (the class default of HorizontalTabList is 10 / 10, the screen overrides it)
            let tab_pad = {
                let store = Store::open(&crate::pak_assets::default_install()).unwrap();
                let pkg = store.pkg("/Game/Athena/UI/Frontend/AthenaTabsScreen").unwrap();
                let e = pkg.exports.iter().find(|e| e.class == "HorizontalTabList_C" && e.path.contains("WidgetArchetype")).unwrap();
                let m = super::store::jmargin(e.props.get("TabButtonPadding"));
                assert_eq!(m, [4.0, 0.0, 4.0, 0.0]);
                m[0] + m[2]
            };
            let mut x_prev_right: Option<f32> = None;
            for (name, r) in &f.tabs {
                let tw = uic.fonts.measure(crate::text::FontId::Bold, name, px_size as f32) / s;
                near(r.w, px(tw + 8.0 + 40.0), &format!("tab {name} width"));
                near(r.h, px(line_h + 8.0), &format!("tab {name} height"));
                near(r.y + r.h * 0.5, px(content_top + list_h * 0.5), &format!("tab {name} vertical centre in the row"));
                if let Some(p) = x_prev_right { near(r.x - p, px(tab_pad), &format!("gap before {name}")); }
                x_prev_right = Some(r.x + r.w);
            }
            // --- AthenaLobby.OverlayMain: area = switcher rect; Launch = OverlaySlot_3 (Right / Bottom, padding R20 B15)
            let area = f.rect("OverlayMain").unwrap();
            near(area.y, px(content_top + list_h), "OverlayMain.y"); near(area.x, 0.0, "OverlayMain.x");
            let launch = f.rect("Launch").unwrap();
            near(launch.x + launch.w, area.x + area.w - px(20.0), "Launch right edge (padding 20)");
            near(launch.y + launch.h, area.y + area.h - px(15.0), "Launch bottom edge (padding 15)");
            // --- AthenaMatchmakingWidget: SizeBox_0 397 x 66 (overrides) inside VerticalBoxSlot padding L9 T10 R10 B10 -> PlayButton fills it
            let play = f.rect("PlayButton").unwrap();
            near(play.w, px(397.0), "PlayButton.w"); near(play.h, px(66.0), "PlayButton.h");
            near(play.x + play.w, launch.x + launch.w - px(10.0), "PlayButton right (slot padding 10)");
            near(play.y + play.h, launch.y + launch.h - px(10.0), "PlayButton bottom (slot padding 10)");
            near(play.x, launch.x + px(9.0), "PlayButton left (slot padding 9)");
            // --- rotators: MinWidth 200 / MinHeight 45, HorizontalBoxSlot HAlign_Right + Fill: right edge = Overlay_1 inner right (padding 10)
            let pr = f.rect("PlaylistRotator").unwrap();
            let fr = f.rect("TeamFillRotator").unwrap();
            for (n, r) in [("PlaylistRotator", pr), ("TeamFillRotator", fr)] {
                near(r.w, px(200.0), &format!("{n}.w")); near(r.h, px(45.0), &format!("{n}.h"));
                near(r.x + r.w, launch.x + launch.w - px(10.0), &format!("{n} right edge"));
            }
            assert!(fr.y > pr.y + pr.h - 1.0, "TeamFillRow below the playlist row");
            // the playlist row is the first row of Overlay_1 (padding 10): top = Launch.y + 10 (+ HorizontalBox_3 first child; PlaylistRotator slot padding bottom 5)
            near(pr.y, launch.y + px(10.0), "PlaylistRotator.y");
            near(fr.y, pr.y + px(45.0 + 5.0 + 5.0 + 5.0), "TeamFillRotator.y (row 1 = 45 + rotator slot padding bottom 5; TeamFillRow slot padding top 5; its rotator slot padding top 5)");
            // --- PlayerPanel: OverlaySlot_6 (Centre / Bottom, padding bottom 134), SizeBox_1 MinDesiredWidth 512; Hidden until a player pad is hovered
            let pp = f.rect("PlayerPanel").unwrap();
            assert!(pp.w >= px(512.0) - 0.5, "PlayerPanel min width 512, got {}", pp.w / s);
            near(pp.x + pp.w * 0.5, w * 0.5, "PlayerPanel centred");
            near(pp.y + pp.h, area.y + area.h - px(134.0), "PlayerPanel bottom (padding 134)");
            // --- season panel: OverlaySlot_1 padding L32 T32, SizeBox_1 WidthOverride 464
            let sp = f.rect("SeasonAndChallengesPanel").unwrap();
            near(sp.x, px(32.0), "SeasonAndChallengesPanel.x"); near(sp.y, area.y + px(32.0), "SeasonAndChallengesPanel.y"); near(sp.w, px(464.0), "SeasonAndChallengesPanel.w");
            let _ = vw;
        }
    }) else { return };
}

#[test]
fn settled_state_defaults_come_from_the_data() {
    let Some(()) = with_lobby(|l, uic| {
        let d = l.dump(uic, 1920.0, 1080.0, &LobbyState::default());
        // runtime-driven widgets are in their settled state: no matchmaking block, no cancel button, player panel hidden, top-bar menu closed
        assert!(d.contains("PlayerPanel") && d.contains("Hidden"));
        let play = d.lines().find(|x| x.contains("\"PlayButton\"")).unwrap();
        assert!(play.contains("Visible"));
        assert!(d.lines().any(|x| x.contains("CenterButtonTextWidget") && x.contains("text=\"Play\"")));
        assert!(d.lines().any(|x| x.contains("MyText") && x.contains("text=\"Solo\"")));
        assert!(d.lines().any(|x| x.contains("MyText") && x.contains("text=\"No Fill\"")));
        let st = LobbyState { playlist: 2, team_fill: 1, ..LobbyState::default() };
        let d2 = l.dump(uic, 1920.0, 1080.0, &st);
        assert!(d2.lines().any(|x| x.contains("MyText") && x.contains("text=\"Squad\"")));
        assert!(d2.lines().any(|x| x.contains("MyText") && x.contains("text=\"Fill\"")));
    }) else { return };
}

#[test]
fn locres_strings_match_the_serialized_source_text() {
    let Some(inst) = install() else { return };
    let store = Store::open(&inst).unwrap();
    let l = store.locres().expect("Game.locres");
    assert_eq!(l.len(), 32721);
    assert_eq!(l.get("[5A092F04442EA1D66BCA0CAF7F084164]", "F797DA54470C13229CE26A91CDB270BE"), Some("Lobby"));
    assert_eq!(l.get("Fortnite.FortAthenaMatchmakingWidget", "SquadPlaylistSolo"), Some("Solo"));
    assert_eq!(l.get("Fortnite.FortAthenaMatchmakingWidget", "SquadPlaylistDuo"), Some("Duo"));
    assert_eq!(l.get("Fortnite.FortAthenaMatchmakingWidget", "SquadPlaylistSquad"), Some("Squad"));
    assert_eq!(l.get("[570C64F344756CC8FFA9A3B51340E2A4]", "FAD003004CD43D8DB49EBE96C9CB08C2"), Some("Play"));
}




/// Bottom bar `InputReflector` shows exactly the News action (InputActionDataTable `AthenaNews`: Enabled for keyboard + mouse, DisplayName "News"); the chat
/// shortcut switcher is Hidden for keyboard + mouse (no "{Text}" / key prompt); pad widgets sit at `anchor - desired size * Pivot` (UWidgetComponent screen-space rule).
#[test]
fn bottom_bar_news_button_and_pad_widgets() {
    let Some(()) = with_lobby(|l, uic| {
        let d = l.dump(uic, 1920.0, 1080.0, &LobbyState::default());
        assert!(d.lines().any(|x| x.contains("CenterButtonTextWidget") && x.contains("text=\"News\"")), "News button missing");
        assert!(d.lines().any(|x| x.contains("\"CursorModeSwitcher\"") && x.contains("Hidden")), "chat shortcut switcher must be Hidden for keyboard + mouse");
        let st = LobbyState { pads: vec![
            Pad { x: 960.0, y: 300.0, pivot: [0.5, 1.0], kind: PadKind::Player { name: Some("Name".into()), leader: true, ready: false } },
            Pad { x: 600.0, y: 600.0, pivot: [0.5, 0.5], kind: PadKind::Empty } ], ..LobbyState::default() };
        let f = l.draw(uic, 1920.0, 1080.0, &st);
        assert_eq!(f.pads.len(), 2);
        // name plate: Pivot (0.5, 1.0) = bottom centre on the anchor; "+" marker: Pivot (0.5, 0.5) = centre
        near(f.pads[0].x + f.pads[0].w * 0.5, 960.0, "plate centre x"); near(f.pads[0].y + f.pads[0].h, 300.0, "plate bottom y");
        near(f.pads[1].x + f.pads[1].w * 0.5, 600.0, "plus centre x"); near(f.pads[1].y + f.pads[1].h * 0.5, 600.0, "plus centre y");
        assert!(f.missing.is_empty(), "{:?}", f.missing);
    }) else { return };
}


/// AthenaRotator Construct (Kismet, mouse + keyboard path): `ButtonRight.SetRenderTransform(Scale(-1, 1))`; ButtonStyle-Right uses the left arrow art.
#[test]
fn rotator_right_buttons_are_mirrored() {
    let Some(()) = with_lobby(|l, _| {
        let n = l.ui.w.iter().filter(|w| w.name == "ButtonRight" && w.transform.scale == [-1.0, 1.0]).count();
        assert_eq!(n, 2 + l.option_rows().len(), "every rotator's ButtonRight must carry the mirror transform");
        assert!(l.ui.w.iter().filter(|w| w.name == "ButtonLeft").all(|w| w.transform.scale == [1.0, 1.0]));
    }) else { return };
}

/// Native `FortAthenaMatchmakingWidget` refresh (exe 0x2eb0a20): Squad enables `TeamFillRotator`; Solo / Duo force the index (No Fill / Fill) and disable it; a
/// non-leader has both rotators disabled.
#[test]
fn squad_fill_rotator_is_disabled_unless_squad() {
    use super::team_fill_rule;
    assert_eq!(team_fill_rule(0, 1, true), (0, false));
    assert_eq!(team_fill_rule(1, 0, true), (1, false));
    assert_eq!(team_fill_rule(2, 1, true), (1, true));
    assert_eq!(team_fill_rule(2, 0, true), (0, true));
    assert_eq!(team_fill_rule(2, 0, false), (0, false));
    let Some(()) = with_lobby(|l, uic| {
        for (playlist, enabled) in [(0usize, false), (1, false), (2, true)] {
            let _ = l.draw(uic, 1920.0, 1080.0, &LobbyState { playlist, ..LobbyState::default() });
            let fr = l.ui.find_by_name("TeamFillRotator").unwrap();
            let pr = l.ui.find_by_name("PlaylistRotator").unwrap();
            assert_eq!(l.ui.w[fr].enabled, enabled, "playlist {playlist}");
            assert!(l.ui.w[pr].enabled);
        }
    }) else { return };
}

/// `MTXButton`: `IconTextButton::SetControllerStyle` -> `UpdateStyle(false)` re-applies the captured `MouseKeyboardStyle` (serialized RightShade) on keyboard + mouse;
/// `Amount` shows `MtxBalance` (0 by default) and the shaded "+" section is drawn.
#[test]
fn v_bucks_box_keeps_the_right_shade_style_and_shows_the_balance() {
    let Some(()) = with_lobby(|l, uic| {
        let f = l.draw(uic, 1920.0, 1080.0, &LobbyState::default());
        let m = l.ui.find_by_name("MTXButton").unwrap();
        let b = l.ui.var(m, "Button").unwrap();
        assert!(l.ui.button_style_class(b).unwrap().contains("ButtonStyle-OutlineRightShade"));
        let a = l.ui.var(m, "Amount").unwrap();
        assert_eq!(l.ui.w[a].text.as_deref(), Some("0"));
        let _ = f;
        let f = l.draw(uic, 1920.0, 1080.0, &LobbyState { mtx_balance: 1234567, ..LobbyState::default() });
        assert_eq!(l.ui.w[a].text.as_deref(), Some("1,234,567"));
        assert!(f.rect("Amount").is_some());
    }) else { return };
}

#[test]
fn join_code_box_sits_left_of_news_on_the_bottom_bar() {
    let Some(()) = with_lobby(|l, uic| {
        for (w, h) in [(1920.0, 1080.0), (2544.0, 1353.0), (1280.0, 720.0)] {
            let st = LobbyState { join_code_input: Some("AB".into()), ..LobbyState::default() };
            let f = l.draw(uic, w, h, &st);
            let join = l.join_code_widget().expect("join code box");
            let jt = l.ui.var(join, "CenterButtonTextWidget").unwrap();
            assert_eq!(l.ui.w[jt].text.as_deref(), Some("AB_"));
            let news = l.ui.w.iter().find(|x| x.text.as_deref() == Some("News")).map(|x| x.id).expect("news text");
            let (j, n) = (f.id_rects[join], f.id_rects[news]);
            assert!(j.w > 0.0 && j.x + j.w <= n.x && j.x > w * 0.5, "{j:?} {n:?}");
            assert!(j.y < n.y + n.h * 0.5 && n.y + n.h * 0.5 < j.y + j.h && j.y > h * 0.8, "{j:?} {n:?}");
            let f = l.draw(uic, w, h, &LobbyState::default());
            assert_eq!(l.ui.w[jt].text.as_deref(), Some(JOIN_CODE_LABEL));
            assert!(f.id_rects[join].w > 0.0);
        }
    }) else { return };
}

#[test]
fn enhanced_mode_row_is_a_team_fill_row_copy_under_it() {
    let Some(()) = with_lobby(|l, uic| {
        let mut st = LobbyState::default();
        let f = l.draw(uic, 1920.0, 1080.0, &st);
        let &(row, rot) = l.option_rows().first().expect("option row");
        let fill = l.ui.find_by_name("TeamFillRow").unwrap();
        assert_eq!(l.ui.w[row].parent, l.ui.w[fill].parent);
        assert_eq!(l.ui.w[rot].class, "AthenaRotator_C");
        let (r, fr) = (f.id_rects[row], f.id_rects[fill]);
        assert!(r.y >= fr.y + fr.h - 0.5 && (r.x - fr.x).abs() < 0.5 && (r.w - fr.w).abs() < 0.5 && (r.h - fr.h).abs() < 0.5, "{r:?} {fr:?}");
        let play = f.rect("PlayButton").unwrap();
        let launch = f.rect("Launch").unwrap();
        eprintln!("row {r:?} play {play:?} launch {launch:?}");
        assert!(play.y >= r.y + r.h - 0.5, "{play:?} {r:?}");
        let text = l.ui.var(rot, "MyText").unwrap();
        assert_eq!(l.ui.w[text].text.as_deref(), Some("Off"));
        assert!(l.ui.w[row].kids.iter().any(|&k| l.ui.w[k].text.as_deref() == Some("Enhanced Mode")));
        st.option_mut(ENHANCED_MODE).unwrap().step(1);
        let _ = l.draw(uic, 1920.0, 1080.0, &st);
        assert_eq!(l.ui.w[text].text.as_deref(), Some("On"));
        let right = l.ui.find_in(rot, "ButtonRight").unwrap();
        assert_eq!(crate::screens::athena_locker::controller::Controller::role(l, right), crate::screens::athena_locker::controller::Role::OptionArrow(0, 1));
    }) else { return };
}
