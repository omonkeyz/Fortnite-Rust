//! Interaction tests of the lobby screens against the 1.8 install (skipped when it is not on this machine): hit testing, tab switching, the Locker flow.
use super::controller::{Controller, Role, UiEvent};
use super::*;
use crate::scene::UiContext;
use crate::screens::athena_lobby::{AthenaLobbyUi, Frame, LobbyState};

fn install() -> Option<std::path::PathBuf> {
    let p = crate::pak_assets::default_install();
    if p.exists() { Some(p) } else { eprintln!("skipping: Fortnite install {} not found (set FN_ROOT)", p.display()); None }
}

const W: f32 = 1920.0;
const H: f32 = 1080.0;

struct Rig { lobby: AthenaLobbyUi, uic: UiContext, ctl: Controller, st: LobbyState, frame: Frame }
impl Rig {
    fn new() -> Option<Rig> {
        let inst = install()?;
        let mut uic = UiContext::from_install(&inst, None).expect("ui context");
        let mut lobby = AthenaLobbyUi::load(&inst).expect("lobby ui");
        lobby.set_card_budget(0); // geometry only: the card bakes are not under test
        let mut st = LobbyState::default();
        // owned items of the offline profile, as the glue supplies them (names / rarities are what the picker needs; no icons)
        let it = |id: &str, r: usize| LockerItem { id: id.into(), name: id.into(), rarity: r, icon: None };
        st.locker.items[0] = (0..12).map(|i| it(&format!("outfit{i}"), i % 4)).collect();
        st.locker.items[1] = vec![it("pickaxe0", 0), it("pickaxe1", 1)];
        st.locker.items[2] = vec![it("glider0", 0)];
        st.locker.equipped = [Some(0), Some(0), Some(0)];
        let frame = lobby.draw(&mut uic, W, H, &st);
        Some(Rig { lobby, uic, ctl: Controller::new(), st, frame })
    }
    fn redraw(&mut self) { self.frame = self.lobby.draw(&mut self.uic, W, H, &self.st); }
    fn move_to(&mut self, p: (f32, f32)) { self.ctl.pointer_move(&self.lobby, &self.frame, &mut self.st, p.0, p.1); self.redraw(); }
    fn click(&mut self, p: (f32, f32)) -> Vec<UiEvent> {
        self.move_to(p);
        self.ctl.pointer_button(&self.lobby, &self.frame, &mut self.st, p.0, p.1, true);
        self.redraw();
        self.ctl.pointer_button(&self.lobby, &self.frame, &mut self.st, p.0, p.1, false);
        self.redraw();
        self.ctl.take_events()
    }
    fn center_of(&self, id: usize) -> (f32, f32) { let r = self.frame.id_rects[id]; (r.x + r.w * 0.5, r.y + r.h * 0.5) }
    fn tab_center(&self, label: &str) -> (f32, f32) { let (_, r) = self.frame.tabs.iter().find(|(n, _)| n == label).unwrap_or_else(|| panic!("no tab {label}: {:?}", self.frame.tabs.iter().map(|t| &t.0).collect::<Vec<_>>())); (r.x + r.w * 0.5, r.y + r.h * 0.5) }
    fn animate(&mut self, secs: f32) { self.ctl.tick(&mut self.st, secs); self.redraw(); }
}

/// Clicking a tab button switches `TopTabContentWidgetSwitcher` to the page of that registration (and only the active page is hit-testable).
#[test]
fn clicking_the_tabs_switches_the_pages() {
    let Some(mut r) = Rig::new() else { return };
    assert_eq!(r.st.selected_tab, 0);
    let play = r.lobby.ui.find_by_name("PlayButton").expect("PlayButton");
    // the Lobby page is up: the pointer over Play hits the Play button
    let pc = r.center_of(play);
    r.move_to(pc);
    assert_eq!(r.st.hover_widget.map(|id| Controller::role(&r.lobby, id)), Some(Role::Play));
    assert!(r.st.play_hovered);
    // click LOCKER
    let ev = r.click(r.tab_center("LOCKER"));
    let locker_tab = r.lobby.tab_labels().iter().position(|l| l == "LOCKER").unwrap();
    assert_eq!(ev.iter().filter(|e| matches!(e, UiEvent::TabSelected(i) if *i == locker_tab)).count(), 1, "{ev:?}");
    assert_eq!(r.st.selected_tab, locker_tab);
    assert!(r.frame.rect("CharacterSlot").is_some(), "the Locker page was composed");
    // the Lobby page is no longer under the pointer: Play is not hit any more
    r.move_to(pc);
    assert_ne!(r.st.hover_widget.map(|id| Controller::role(&r.lobby, id)), Some(Role::Play));
    assert!(!r.st.play_hovered);
    // and back
    let ev = r.click(r.tab_center("LOBBY"));
    assert!(ev.contains(&UiEvent::TabSelected(0)));
    assert_eq!(r.st.selected_tab, 0);
    r.move_to(pc);
    assert_eq!(r.st.hover_widget.map(|id| Controller::role(&r.lobby, id)), Some(Role::Play));
    // the hovered tab gets the hovered button state (and the selected tab keeps its selected one)
    let lobby_tab = r.lobby.tab_widgets()[0];
    let other = r.lobby.tab_widgets()[1];
    let oc = r.center_of(other);
    r.move_to(oc);
    use crate::screens::athena_lobby::styles::BtnState;
    assert_eq!(r.lobby.ui.w[other].btn_state, BtnState::Hovered);
    assert_eq!(r.lobby.ui.w[lobby_tab].btn_state, BtnState::Selected);
}

/// Every registered tab can be selected and drawn (pages that cannot be composed stay empty instead of panicking).
#[test]
fn every_tab_can_be_selected() {
    let Some(mut r) = Rig::new() else { return };
    for i in 0..r.lobby.tab_labels().len() {
        let c = r.center_of(r.lobby.tab_widgets()[i]);
        r.click(c);
        assert_eq!(r.st.selected_tab, i, "tab {i} {}", r.lobby.tab_labels()[i]);
        eprintln!("tab {i} {:?}: content {:?}, page {:?}", r.lobby.tab_labels()[i], r.lobby.tab_content(i), r.lobby.page(i));
    }
}

#[test]
fn locker_slots_open_the_picker_and_a_tile_click_equips() {
    let Some(mut r) = Rig::new() else { return };
    r.click(r.tab_center("LOCKER"));
    let slot = r.lobby.locker().expect("locker page").slot_widget(Slot::Outfit);
    assert_eq!(Controller::role(&r.lobby, slot), Role::Slot(Slot::Outfit));
    // closed picker: no tile is hit-testable, the slot is
    assert!(r.st.locker.picking.is_none());
    let ev = r.click(r.center_of(slot));
    assert!(ev.contains(&UiEvent::PickerOpened(Slot::Outfit)), "{ev:?}");
    assert_eq!(r.st.locker.picking, Some(Slot::Outfit));
    // IntroPicker plays: closed at 0, open at 0.25 s
    assert_eq!(r.st.locker.picker_t, 0.0);
    r.animate(0.3);
    assert!((r.st.locker.picker_t - INTRO_PICKER_END).abs() < 1e-6);
    let tv = r.lobby.locker().unwrap().tile_view().unwrap();
    let tvr = r.frame.id_rects[tv];
    assert!(tvr.x > 0.0 && tvr.x + tvr.w <= W * 0.5, "the open picker covers the left half: {tvr:?}");
    let (cols, vis, total) = r.lobby.locker().unwrap().grid;
    assert!(cols >= 2 && vis >= 1 && total >= 1, "{:?}", r.lobby.locker().unwrap().grid);
    // the header names the category (BeginPickingCustomization: OUTFIT)
    let h = r.lobby.ui.find_by_name("HeaderItemType").unwrap();
    assert_eq!(r.lobby.ui.w[h].text.as_deref(), Some("OUTFIT"));
    // the tile buttons carry the Locker item sounds (their serialized slate sound overrides)
    let tile0 = r.lobby.locker().unwrap().tile_button(Slot::Outfit, 0).unwrap();
    let c0 = r.center_of(tile0);
    r.ctl.take_events();
    r.move_to(c0);
    let ev = r.ctl.take_events();
    assert!(ev.iter().any(|e| matches!(e, UiEvent::HoverSound(s) if s.ends_with("Fort_Athena_LockerItem_Hovered.Fort_Athena_LockerItem_Hovered"))), "{ev:?}");
    r.ctl.pointer_button(&r.lobby, &r.frame, &mut r.st, c0.0, c0.1, true);
    assert_eq!(r.st.press_widget, Some(tile0));
    let ev = r.ctl.take_events();
    assert!(ev.iter().any(|e| matches!(e, UiEvent::PressSound(s) if s.ends_with("Fort_Athena_LockerItem_Pressed.Fort_Athena_LockerItem_Pressed"))), "{ev:?}");
    // released away from the button: no click
    r.ctl.pointer_button(&r.lobby, &r.frame, &mut r.st, 5.0, H - 5.0, false);
    assert!(r.ctl.take_events().iter().all(|e| !matches!(e, UiEvent::Equip { .. })));
    r.st.press_widget = None;
    // click the third item
    let tile = r.lobby.locker().unwrap().tile_button(Slot::Outfit, 2).expect("tile 2");
    let c = r.center_of(tile);
    assert_eq!(Controller::role(&r.lobby, tile), Role::Tile(Slot::Outfit, 2));
    let ev = r.click(c);
    assert!(ev.contains(&UiEvent::Preview { slot: Slot::Outfit, index: 2 }), "{ev:?}");
    assert!(r.st.locker.picking.is_some() && r.st.locker.equipped[0] != Some(2));
    let ev = r.click(c);
    assert!(ev.contains(&UiEvent::Equip { slot: Slot::Outfit, index: 2 }), "{ev:?}");
    assert!(ev.contains(&UiEvent::PickerClosed));
    assert_eq!(r.st.locker.equipped[0], Some(2));
    assert!(r.st.locker.picking.is_none());
    r.animate(0.3);
    assert_eq!(r.st.locker.picker_t, 0.0, "the picker slid back out");
    // Escape while picking ends picking (HandleCancel); without picking it is not consumed
    r.click(r.center_of(slot));
    assert!(r.st.locker.picking.is_some());
    assert!(r.ctl.cancel(&mut r.st));
    assert!(r.st.locker.picking.is_none());
    assert!(!r.ctl.cancel(&mut r.st));
    // the pickaxe and glider slots open their own category
    for (s, name) in [(Slot::Pickaxe, "HARVESTING TOOL"), (Slot::Glider, "GLIDER")] {
        let b = r.lobby.locker().unwrap().slot_widget(s);
        r.click(r.center_of(b));
        assert_eq!(r.st.locker.picking, Some(s));
        r.animate(0.3);
        let h = r.lobby.ui.find_by_name("HeaderItemType").unwrap();
        assert_eq!(r.lobby.ui.w[h].text.as_deref(), Some(name));
        r.ctl.cancel(&mut r.st);
        r.animate(0.3);
    }
}

/// The whole-row scroll of the picker: the mouse wheel moves rows, the grid hides rows outside the view (they are not hit-testable).
#[test]
fn the_picker_scrolls_in_whole_rows() {
    let Some(mut r) = Rig::new() else { return };
    r.st.locker.items[0] = (0..60).map(|i| LockerItem { id: format!("outfit{i}"), name: format!("outfit{i}"), rarity: 0, icon: None }).collect();
    r.click(r.tab_center("LOCKER"));
    let slot = r.lobby.locker().unwrap().slot_widget(Slot::Outfit);
    r.click(r.center_of(slot));
    r.animate(0.3);
    let (cols, vis, total) = r.lobby.locker().unwrap().grid;
    assert!(total > vis, "60 items need scrolling: {:?}", (cols, vis, total));
    let tv = r.lobby.locker().unwrap().tile_view().unwrap();
    let c = r.center_of(tv);
    let before = r.st.locker.scroll_row;
    r.ctl.wheel(&r.lobby, &r.frame, &mut r.st, c.0, c.1, 1);
    assert_eq!(r.st.locker.scroll_row, before + 1);
    r.redraw();
    // the first row is hidden now, the first item of the second row sits where the first row was
    let t0 = r.lobby.locker().unwrap().tile_button(Slot::Outfit, 0).unwrap();
    assert!(!r.lobby.ui.w[t0].vis.draws());
    let first_visible = r.lobby.locker().unwrap().tile_button(Slot::Outfit, cols).unwrap();
    assert!(r.lobby.ui.w[first_visible].vis.draws());
    // wheel outside the picker does nothing
    r.ctl.wheel(&r.lobby, &r.frame, &mut r.st, W - 10.0, H - 10.0, 1);
    assert_eq!(r.st.locker.scroll_row, before + 1);
    // clamps at the end
    for _ in 0..200 { r.ctl.wheel(&r.lobby, &r.frame, &mut r.st, c.0, c.1, 1); }
    assert_eq!(r.st.locker.scroll_row, total - vis);
}

#[test]
fn the_banner_slot_opens_the_editor_and_apply_sets_the_banner() {
    let Some(mut r) = Rig::new() else { return };
    r.click(r.tab_center("LOCKER"));
    let l = r.lobby.locker().unwrap();
    assert!(l.banner.data.icons.len() > 100 && l.banner.data.colors.len() == 21, "{} {}", l.banner.data.icons.len(), l.banner.data.colors.len());
    assert_eq!(l.banner.data.icon_cats.first().map(|c| c.0.as_str()), Some("Standard"));
    let slot = l.banner_slot.expect("BannerSlot");
    assert_eq!(Controller::role(&r.lobby, slot), Role::BannerSlot);
    let sr = r.frame.id_rects[slot];
    let img = r.lobby.ui.w.iter().filter(|w| w.name == "BannerImage" && r.lobby.ui.is_descendant(w.id, slot)).map(|w| w.id).next().expect("banner image");
    let ir = r.frame.id_rects[img];
    assert!((ir.w / ir.h - 0.8).abs() < 0.02 && ir.w <= sr.w + 0.5, "{ir:?} in {sr:?}");
    assert!(r.lobby.ui.w[img].mat_textures.iter().any(|(k, v)| k == "BannerIcon" && v.contains("Arrow")), "{:?}", r.lobby.ui.w[img].mat_textures);
    r.click(r.center_of(slot));
    let e = r.st.locker.banner_editor.clone().expect("editor open");
    assert!(!e.color_page && e.icon == banner::DEFAULT_ICON);
    r.redraw();
    let l = r.lobby.locker().unwrap();
    let (cols, vis, total) = l.banner.grid;
    assert!(cols >= 4 && vis >= 2 && total >= 4, "{:?}", l.banner.grid);
    let modal = l.banner.modal().unwrap();
    let tiles: Vec<usize> = r.lobby.ui.w.iter().filter(|w| r.lobby.ui.is_descendant(w.id, modal) && matches!(Controller::role(&r.lobby, w.id), Role::Banner(banner::BannerButton::Tile(_)))).map(|w| w.id).collect();
    let t1 = *tiles.iter().find(|&&t| Controller::role(&r.lobby, t) == Role::Banner(banner::BannerButton::Tile(1))).unwrap();
    let tr = r.frame.id_rects[t1];
    assert!((tr.w - 96.0).abs() < 1.0 && (tr.h - 120.0).abs() < 1.0 && cols == 6, "{tr:?} {cols} {:?}", r.frame.id_rects[r.lobby.locker().unwrap().banner.tile_view().unwrap()]);
    r.click(r.center_of(t1));
    let e = r.st.locker.banner_editor.clone().unwrap();
    assert_ne!(e.icon, banner::DEFAULT_ICON);
    let apply = r.lobby.ui.find_in(modal, "ApplyButton").unwrap();
    let ev = r.click(r.center_of(apply));
    assert!(ev.iter().any(|x| matches!(x, UiEvent::BannerApply { icon, .. } if *icon == e.icon)), "{ev:?}");
    assert_eq!(r.st.locker.banner_icon, e.icon);
    let ec = r.lobby.ui.find_in(modal, "EditColorButton").unwrap();
    r.click(r.center_of(ec));
    assert!(r.st.locker.banner_editor.as_ref().unwrap().color_page);
    assert!(r.ctl.cancel(&mut r.st));
    assert!(r.st.locker.banner_editor.is_none());
}

#[test]
fn the_last_picker_row_is_drawn_at_full_tile_size() {
    let Some(mut r) = Rig::new() else { return };
    r.st.locker.items[0] = (0..31).map(|i| LockerItem { id: format!("outfit{i}"), name: format!("outfit{i}"), rarity: i % 4, icon: None }).collect();
    r.click(r.tab_center("LOCKER"));
    let slot = r.lobby.locker().unwrap().slot_widget(Slot::Outfit);
    r.click(r.center_of(slot));
    r.animate(0.3);
    r.redraw();
    let tv = r.lobby.locker().unwrap().tile_view().unwrap();
    let c = r.center_of(tv);
    for _ in 0..50 { r.ctl.wheel(&r.lobby, &r.frame, &mut r.st, c.0, c.1, 1); }
    r.redraw();
    let tvr = r.frame.id_rects[tv];
    let last = r.lobby.locker().unwrap().tile_button(Slot::Outfit, 30).unwrap();
    let first = r.lobby.locker().unwrap().tile_button(Slot::Outfit, 0).unwrap();
    assert!(r.lobby.ui.w[last].vis.draws());
    let (lr, fr) = (r.frame.id_rects[last], r.frame.id_rects[first]);
    let r29 = r.frame.id_rects[r.lobby.locker().unwrap().tile_button(Slot::Outfit, 29).unwrap()];
    assert!(r.lobby.ui.w[r.lobby.locker().unwrap().tile_button(Slot::Outfit, 29).unwrap()].vis.draws() && (lr.h - r29.h).abs() < 0.5 && lr.h > 50.0, "{lr:?} vs {r29:?} first {fr:?}");
    assert!(lr.y + lr.h <= tvr.y + tvr.h + 1.0, "{lr:?} outside {tvr:?}");
}
