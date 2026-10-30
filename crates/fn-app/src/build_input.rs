//! BUILD INPUT: build mode, building piece selection, material tier and the edit keys, as the 1.8 `AFortPlayerController` input components do them.
//! Pure state machine: it reads the bindings once per frame and emits [`BuildIntent`]s; the building core (`building_hook.rs`), the editing session and
//! the build animation owner consume them (`drain_intents`) or poll the public state (`is_building`, `piece`, `material`).
//!
//! SOURCES (1.8 exe, `C:\FortniteRef\decomp\named`; addresses are the exe's):
//! * `AFortPlayerController::SetupInputComponent` @14084a570 binds three input components:
//!   - main (`InputComponent`): `SwitchQuickbar` pressed -> FUN_14083d040; `HotbarKeyF1..F7` pressed -> FUN_14083a690.. (`AFortQuickBars` slot activate on the
//!     Secondary bar, slot N); `HotbarKey1..10` pressed -> Primary bar slot N; `SlotUp` / `SlotDown` -> FUN_14083ce70 / FUN_14083ce10 (next / previous valid slot of
//!     the FOCUSED bar, wrapping: FUN_140684240); `PerformBuildingEditInteraction` pressed (begin edit, see the editing session).
//!   - `BuildPreviewModeInputComponent0` (pushed while a building piece is held): `Fire` pressed -> FUN_1408336f0, released -> FUN_14084ea40; `ChangeMaterial`
//!     pressed -> vtable +0x1550 (`AFortPlayerController::vfunc_682` @140833bc0); `PerformBuildingImprovementInteraction`; `RotatePrimitiveClockwise` /
//!     `RotatePrimitiveCounterClockwise`; `SwitchToSelectedBuildingType`.
//!   - `EditModeInputComponent0` (pushed while editing): `Fire` pressed -> FUN_140834d80 (select tile), released -> LAB_140834d40; `BuildingEditReset` pressed ->
//!     LAB_140834d60; `CompleteBuildingEditInteraction` pressed -> FUN_140837210 (confirm).
//! * SWITCH QUICKBAR (FUN_14083d040): Secondary focused -> focus Primary at its remembered slot; otherwise focus Secondary at its remembered slot, except when
//!   `UFortClientSettingsRecord.bNewFocusOnFirstBuildingPieceWhenQuickbarSwapped` (+0x163; the Settings row "Reset Building Choice") is set and the remembered
//!   slot is a building piece (< 4) -> slot 0. The quickbar keeps `CurrentFocusedSlot` per bar (FUN_140698120 passes -1 = "keep").
//! * TURBO BUILD DOES NOT EXIST IN 1.8: the `Fire` binding of the build preview component is IE_Pressed (0) / IE_Released (1) only, the pressed handler
//!   FUN_1408336f0 performs exactly one `ServerCreateBuildingActor` attempt and nothing else references it (no timer, no repeat). Holding Fire places one piece.
//! * MATERIAL (`vfunc_682`): only while `CurrentResourceType != None(4)`, `CurrentBuildableClass` is an `ABuildingSMActor` and the preview marker is not the
//!   building being edited: next entry of the resource order array (+0x14c8), wrapping, then FUN_140834fc0 sets `CurrentResourceType` and swaps the buildable class.
//!   The order array is filled in the controller ctor @140810140 with {Wood, Stone, Metal} and `CurrentResourceType` = Wood (0). No affordability check: the
//!   cycle never skips a tier you cannot afford (affordability is checked when placing: `vfunc_677` @140815aa0).
//! * NOT ENOUGH MATERIALS (FUN_1408336f0 when `vfunc_677` fails): `ClientSendMessage(Fort.Player.Defaults.UnableToAfford = "Not enough {0} to build! Change
//!   building material or gather more!")` (shown as an `AFortClientAnnouncement_Basic`) + a UI feedback event. The placement path reports it back through
//!   [`BuildInput::unable_to_afford`].
//!
//! DEFAULT KEYS (PC): `DefaultInput.ini` is AES-encrypted, but the cooker's ini value list in pakchunk0 is not
//! (`C:\FortniteRef\scratch\CookedIniVersion.txt`, `Mac.Input:` lines). `/Script/FortniteGame.FortPlayerInput:DefaultPresetNameAthena=ConfigD_Athena`
//! ("Standard"; ConfigE_Athena = "Quick Builder", ConfigF_Athena = "Combat Pro" are the controller presets) maps, on keyboard/mouse:
//! SwitchQuickbar Q / ThumbMouseButton, HotbarKeyF1..F7 = F1..F7, Fire LeftMouseButton, ChangeMaterial RightMouseButton, BuildingEditReset RightMouseButton,
//! PerformBuildingEditInteraction G, CompleteBuildingEditInteraction G, RotatePrimitiveClockwise R, PerformBuildingImprovementInteraction F / ThumbMouseButton2,
//! SwitchToSelectedBuildingType V, SlotUp MouseScrollDown, SlotDown MouseScrollUp, ToggleInventory I, ToggleFullScreenMap M / Tab; HoldInputTime 0.25,
//! bEditModeUsableFromCombatMode true (edit works from the combat bar too). These are the `fn_settings` binding-table defaults (generated from
//! CookedIniVersion.txt by tools/settings-evidence.cjs; shared keys across input groups are allowed there like in 1.8), so this module reads the
//! player's bindings only: a row the player cleared stays unbound, as in 1.8.
//! `CompleteBuildingEditInteraction` has no row of its own in the 1.8 key-binding list (`ConstructKeyBindList`); it shares G with the "Building Edit" row
//! in ConfigD_Athena, so it reads that row here.
use fn_building::pieces::{Material, PieceKind};
use fn_settings::{Bindings, A_BUILDING_EDIT_RESET, A_CHANGE_MATERIAL, A_FIRE, A_PERFORM_BUILDING_EDIT_INTERACTION, A_PERFORM_BUILDING_IMPROVEMENT_INTERACTION, A_ROTATE,
    A_SECONDARY_QUICKBAR_SLOT_F1, A_SLOT_DOWN, A_SLOT_UP, A_SWITCH_QUICKBAR};

/// Secondary quickbar slots: F1..F4 = the four pieces (BuildingItemData_Wall / _Floor / _Stair_W / _RoofS), F5 = the trap slot.
pub const BUILD_SLOTS: usize = 5;
pub const PIECE_SLOTS: usize = 4;

/// What the input layer asks the building systems to do this frame (in the order the keys were handled).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BuildIntent {
    /// The Secondary quickbar got the focus (build mode entered) with this piece selected.
    EnterBuildMode(PieceKind),
    /// The Primary quickbar got the focus (build mode left).
    ExitBuildMode,
    /// Another piece was selected while in build mode.
    SelectPiece(PieceKind),
    /// `ChangeMaterial`: the new tier.
    MaterialChanged(Material),
    /// `Fire` pressed in build mode: one placement attempt (no turbo build in 1.8).
    Place,
    /// `RotatePrimitiveClockwise`.
    Rotate,
    /// `PerformBuildingImprovementInteraction` (repair / upgrade the targeted building).
    Improve,
    /// `PerformBuildingEditInteraction` pressed while not editing.
    EditBegin,
    /// Edit mode: `Fire` pressed / released (begin / end the tile select action).
    EditSelectPressed,
    EditSelectReleased,
    /// Edit mode: `BuildingEditReset`.
    EditReset,
    /// Edit mode: `CompleteBuildingEditInteraction`.
    EditConfirm,
    EditCancel,
}

pub fn edit_switch_cancel_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| fn_live::var("FN_EDIT_SWITCH_CANCEL").map_or(true, |v| v != "0"))
}

/// The build-mode input state machine (one per local player).
#[derive(Clone, Debug)]
pub struct BuildInput {
    /// FocusedQuickbar == Secondary.
    focused_secondary: bool,
    /// `CurrentFocusedSlot` of the Secondary bar (remembered while the Primary bar is focused).
    building_slot: usize,
    /// `CurrentResourceType`.
    material: Material,
    /// The editing session is running (set by the edit hook: `set_editing`).
    editing: bool,
    /// Which secondary slots hold an item (the trap slot is empty in the sandbox).
    occupied: [bool; BUILD_SLOTS],
    prev: [bool; 16],
    intents: Vec<BuildIntent>,
    /// (material, seconds left) of the last "not enough materials" report.
    afford: Option<(Material, f32)>,
    status: Option<fn_ui::screens::hud_cooked::StatusEvent>,
    /// The intents of the last finished frame (`end_frame`), for consumers that poll instead of draining.
    last: Vec<BuildIntent>,
    /// `Gameplay.Action.Player.InteractBuild` is blocked by an active ability (the pawn ASC's `AreAbilityTagsBlocked`; the app sets it from the pickaxe combo,
    /// `fn_combat::Swing::blocks_interact_build`, before the building hook runs this frame's intents).
    interact_build_blocked: bool,
    /// How many `PerformBuildingEditInteraction` presses were refused by that block (`Building.Edit.Failed` feedback, "Unable to Edit Building").
    edit_failed: u32,
    trap_icon: Option<(String, u8)>,
}
impl Default for BuildInput { fn default() -> Self { Self::new() } }

/// Edge-detection slots of `prev`.
const K_SWITCH: usize = 0; const K_F1: usize = 1; // .. K_F1 + 4
const K_FIRE: usize = 6; const K_MAT: usize = 7; const K_EDIT: usize = 8; const K_RESET: usize = 9; const K_ROT: usize = 10; const K_IMPROVE: usize = 11; const K_UP: usize = 12; const K_DOWN: usize = 13;

/// AFortClientAnnouncement `TimeToLive` of the generic basic announcement (`Announce_Gen_Basic` CDO: 5.0 s).
pub const UNABLE_TO_AFFORD_SECONDS: f32 = 5.;

/// The keys used for `action`: the player's binding row (defaults = ConfigD_Athena via the generated `fn_settings` table).
pub fn keys_for(b: &Bindings, action: usize) -> [fn_settings::Key; 2] { b.get(action) }
fn down(b: &Bindings, action: usize, poll: &impl Fn(u16) -> bool) -> bool { keys_for(b, action).iter().filter_map(|k| k.poll_code()).any(|c| poll(c)) }

pub fn piece_of_slot(slot: usize) -> Option<PieceKind> { PieceKind::ALL.get(slot).copied() }

impl BuildInput {
    /// Controller ctor state: Primary bar focused, Secondary remembered slot 0 (Wall), Wood.
    pub fn new() -> Self { BuildInput { focused_secondary: false, building_slot: 0, material: Material::Wood, editing: false, occupied: [true, true, true, true, false], prev: [false; 16], intents: Vec::new(), afford: None, status: None, last: Vec::new(), interact_build_blocked: false, edit_failed: 0, trap_icon: None } }

    pub fn is_building(&self) -> bool { self.focused_secondary }
    pub fn is_editing(&self) -> bool { self.editing }
    pub fn building_slot(&self) -> usize { self.building_slot }
    /// The held piece (None while the Primary bar is focused or the trap slot is selected).
    pub fn piece(&self) -> Option<PieceKind> { if self.focused_secondary { piece_of_slot(self.building_slot) } else { None } }
    pub fn material(&self) -> Material { self.material }
    pub fn trap_held(&self) -> bool { self.focused_secondary && self.building_slot == PIECE_SLOTS && self.occupied[PIECE_SLOTS] }
    pub fn set_trap_icon(&mut self, icon: Option<(String, u8)>) { self.trap_icon = icon; }
    /// Set by the trap / inventory side when the F5 slot gets an item.
    pub fn set_slot_occupied(&mut self, slot: usize, occupied: bool) { if slot < BUILD_SLOTS { self.occupied[slot] = occupied; } }
    /// This frame's `Gameplay.Action.Player.InteractBuild` block (see the field); the app sets it every frame before the building hook.
    pub fn set_interact_build_blocked(&mut self, blocked: bool) { self.interact_build_blocked = blocked; }
    pub fn interact_build_blocked(&self) -> bool { self.interact_build_blocked }
    /// Refused edit presses so far (each one is a `Building.Edit.Failed` feedback in 1.8).
    pub fn edit_failed_count(&self) -> u32 { self.edit_failed }
    /// `AFortPlayerController::vfunc_669` @14083e780 (the `PerformBuildingEditInteraction` binding, SetupInputComponent @14084a570 -> LAB_140812220 -> PC slot 669):
    /// PC slot 620 asks the pawn (slot 338 @1407d0730 -> ASC +0xC40, FUN_14028fb20) `!AreAbilityTagsBlocked({InteractBuild})` (GlobalTags +0x2c0). Allowed ->
    /// begin / end the InteractBuild action and `FUN_1408316c0(PC, 3)` -> ServerBeginEditingBuildingActor (the edit tool is equipped). Blocked -> nothing else
    /// happens: `FUN_14080c350(pawn, 1, {Building.Edit.Failed})` (GlobalTags +0x4c0) shows "Unable to Edit Building" through the feedback manager; the press is
    /// not buffered (the player presses again once the ability has ended). Returns whether the edit may begin.
    pub fn try_interact_build(&mut self) -> bool {
        if !self.interact_build_blocked { return true; }
        self.edit_failed += 1;
        self.status = Some(fn_ui::screens::hud_cooked::StatusEvent::BuildingEditFailed);
        eprintln!("[build-input] PerformBuildingEditInteraction refused: Gameplay.Action.Player.InteractBuild blocked by the active pickaxe combo -> Building.Edit.Failed \"Unable to Edit Building\" (#{})", self.edit_failed);
        false
    }
    /// The editing session started / ended (edit hook): switches the Fire / reset / confirm keys to the edit input component.
    pub fn set_editing(&mut self, editing: bool) { self.editing = editing; }

    /// Focus the Secondary bar (build mode). `slot` None = the remembered slot (FUN_14083d040 with the Reset Building Choice rule applied by the caller).
    pub fn enter_build_mode(&mut self, slot: Option<usize>) {
        let slot = slot.unwrap_or(self.building_slot);
        if slot >= BUILD_SLOTS || !self.occupied[slot] { return; } // FUN_140698930: an invalid slot is not focused
        let changed = self.building_slot != slot;
        self.building_slot = slot;
        if !self.focused_secondary {
            self.focused_secondary = true;
            if let Some(p) = piece_of_slot(slot) { self.intents.push(BuildIntent::EnterBuildMode(p)); }
        } else if changed { if let Some(p) = piece_of_slot(slot) { self.intents.push(BuildIntent::SelectPiece(p)); } }
    }
    /// Focus the Primary bar (a weapon slot key, SwitchQuickbar from build mode, or the game forcing it: `ClientForceCancelBuildingTool`).
    pub fn exit_build_mode(&mut self) {
        if self.editing {
            if !edit_switch_cancel_enabled() { return; }
            self.cancel_edit();
        }
        if self.focused_secondary { self.focused_secondary = false; self.intents.push(BuildIntent::ExitBuildMode); }
    }
    fn cancel_edit(&mut self) {
        if !self.editing { return; }
        self.editing = false;
        self.intents.push(BuildIntent::EditCancel);
    }
    /// A Primary bar slot key was pressed (the existing quickbar input handles the slot itself).
    pub fn on_primary_slot_pressed(&mut self) { self.exit_build_mode(); }

    /// `ChangeMaterial` (vfunc_682): only with a building piece held and not while editing; Wood -> Stone -> Metal -> Wood.
    pub fn change_material(&mut self) {
        if self.piece().is_none() || self.editing { return; }
        self.material = self.material.next();
        self.intents.push(BuildIntent::MaterialChanged(self.material));
    }
    /// The placement path found the current tier unaffordable (FUN_1408336f0): starts the "Not enough {0} to build!" message.
    pub fn unable_to_afford(&mut self, material: Material) { self.afford = Some((material, UNABLE_TO_AFFORD_SECONDS)); self.status = Some(fn_ui::screens::hud_cooked::StatusEvent::UnableToAfford(match material { Material::Wood => 0, Material::Stone => 1, Material::Metal => 2 })); }
    pub fn take_status(&mut self) -> Option<fn_ui::screens::hud_cooked::StatusEvent> { self.status.take() }
    /// The material of a still-showing "not enough materials" message.
    pub fn afford_message(&self) -> Option<Material> { self.afford.map(|a| a.0) }

    pub fn drain_intents(&mut self) -> Vec<BuildIntent> { std::mem::take(&mut self.intents) }
    pub fn peek_intents(&self) -> &[BuildIntent] { &self.intents }
    /// Close the frame: the pending intents (not drained by a hook this frame) become `last_intents`; nothing accumulates across frames.
    pub fn end_frame(&mut self) { self.last = std::mem::take(&mut self.intents); }
    pub fn last_intents(&self) -> &[BuildIntent] { &self.last }

    /// One frame of keys. `poll` answers a binding key code (the same poll as the rest of the input). `reset_building_choice` = settings `game.reset_building_choice`.
    pub fn update(&mut self, b: &Bindings, poll: impl Fn(u16) -> bool, reset_building_choice: bool, dt: f32) {
        if let Some(a) = self.afford.as_mut() { a.1 -= dt.max(0.); if a.1 <= 0. { self.afford = None; } }
        let edge = |k: usize, action: usize, prev: &mut [bool; 16]| -> (bool, bool) { let d = down(b, action, &poll); let p = prev[k]; prev[k] = d; (d && !p, !d && p) };
        let mut prev = self.prev;
        let (switch, _) = edge(K_SWITCH, A_SWITCH_QUICKBAR, &mut prev);
        let slots: [bool; BUILD_SLOTS] = std::array::from_fn(|i| edge(K_F1 + i, A_SECONDARY_QUICKBAR_SLOT_F1 + i, &mut prev).0);
        let (fire_p, fire_r) = edge(K_FIRE, A_FIRE, &mut prev);
        let (mat, _) = edge(K_MAT, A_CHANGE_MATERIAL, &mut prev);
        let (edit, _) = edge(K_EDIT, A_PERFORM_BUILDING_EDIT_INTERACTION, &mut prev);
        let (reset, _) = edge(K_RESET, A_BUILDING_EDIT_RESET, &mut prev);
        let (rot, _) = edge(K_ROT, A_ROTATE, &mut prev);
        let (improve, _) = edge(K_IMPROVE, A_PERFORM_BUILDING_IMPROVEMENT_INTERACTION, &mut prev);
        let (up, _) = edge(K_UP, A_SLOT_UP, &mut prev);
        let (down, _) = edge(K_DOWN, A_SLOT_DOWN, &mut prev);
        self.prev = prev;

        let slot_key = (0..BUILD_SLOTS).any(|i| slots[i] && self.occupied[i]);
        let wheel = self.focused_secondary && (up || down) && !(up && down);
        if self.editing && edit_switch_cancel_enabled() && (switch || slot_key || wheel) { self.cancel_edit(); }
        if self.editing {
            // EditModeInputComponent0 is on top of the stack: Fire / reset / confirm go to the session.
            if fire_p { self.intents.push(BuildIntent::EditSelectPressed); }
            if fire_r { self.intents.push(BuildIntent::EditSelectReleased); }
            if reset { self.intents.push(BuildIntent::EditReset); }
            if edit { self.intents.push(BuildIntent::EditConfirm); } // CompleteBuildingEditInteraction = G (decrypted DefaultInput.ini line 252, same key as PerformBuildingEditInteraction)
            return;
        }
        if switch {
            if self.focused_secondary { self.exit_build_mode(); }
            else {
                let slot = if reset_building_choice && self.building_slot < PIECE_SLOTS { 0 } else { self.building_slot };
                self.enter_build_mode(Some(slot));
            }
        }
        if let Some(i) = (0..BUILD_SLOTS).filter(|&i| slots[i]).last() { self.enter_build_mode(Some(i)); }
        if self.focused_secondary && (up || down) && !(up && down) {
            // FUN_140684240: next / previous VALID slot of the focused bar, wrapping
            let dir = if up { 1 } else { BUILD_SLOTS - 1 };
            let mut s = self.building_slot;
            for _ in 0..BUILD_SLOTS { s = (s + dir) % BUILD_SLOTS; if self.occupied[s] { break; } }
            self.enter_build_mode(Some(s));
        }
        if mat { self.change_material(); }
        if self.piece().is_some() || self.trap_held() {
            if fire_p { self.intents.push(BuildIntent::Place); }
            if rot { self.intents.push(BuildIntent::Rotate); }
            if improve { self.intents.push(BuildIntent::Improve); }
        }
        if edit { self.intents.push(BuildIntent::EditBegin); }
    }

    /// The building quickbar HUD state (key texts from the bindings).
    pub fn hud(&self, b: &Bindings) -> fn_ui::screens::build_quickbar::BuildBarState {
        let key = |action: usize| { let k = keys_for(b, action)[0]; if k.name() == "None" { String::new() } else { k.name().to_owned() } };
        let mut st = fn_ui::screens::build_quickbar::BuildBarState { focused_secondary: self.focused_secondary, building_slot: self.building_slot, switch_key: key(A_SWITCH_QUICKBAR), ..Default::default() };
        for i in 0..BUILD_SLOTS { st.slot_keys[i] = key(A_SECONDARY_QUICKBAR_SLOT_F1 + i); if !self.occupied[i] { st.icons[i] = None; } }
        if self.occupied[PIECE_SLOTS] { if let Some((icon, rarity)) = &self.trap_icon { st.icons[PIECE_SLOTS] = Some(icon.clone()); st.rarity[PIECE_SLOTS] = *rarity; } }
        st
    }
    /// `PackResource` highlight (`GetCurrentResourceItemDefinition`).
    pub fn hud_resource(&self) -> fn_ui::screens::reticle_state::HarvestResource {
        use fn_ui::screens::reticle_state::HarvestResource as R;
        match self.material { Material::Wood => R::Wood, Material::Stone => R::Stone, Material::Metal => R::Metal }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fn_settings::{Key, Settings};
    use std::cell::RefCell;
    fn setup() -> (BuildInput, Bindings) {
        let b = Settings::default().input.bindings.clone(); // the ConfigD_Athena defaults
        (BuildInput::new(), b)
    }
    fn press(bi: &mut BuildInput, b: &Bindings, keys: &[&str]) -> Vec<BuildIntent> {
        let down: Vec<u16> = keys.iter().map(|k| Key::from_name(k).unwrap().poll_code().unwrap()).collect();
        let d = RefCell::new(down);
        bi.update(b, |c| d.borrow().contains(&c), false, 1. / 60.);
        bi.update(b, |_| false, false, 1. / 60.); // release
        bi.drain_intents()
    }
    #[test] fn spawn_state_is_combat_wall_wood() { let bi = BuildInput::new(); assert!(!bi.is_building() && bi.building_slot() == 0 && bi.material() == Material::Wood && bi.piece().is_none()); }
    #[test] fn switch_quickbar_toggles_and_remembers_the_piece() {
        let (mut bi, b) = setup();
        assert_eq!(press(&mut bi, &b, &["Q"]), vec![BuildIntent::EnterBuildMode(PieceKind::Wall)]);
        assert_eq!(press(&mut bi, &b, &["F3"]), vec![BuildIntent::SelectPiece(PieceKind::Stairs)]);
        assert_eq!(press(&mut bi, &b, &["Q"]), vec![BuildIntent::ExitBuildMode]);
        assert_eq!(press(&mut bi, &b, &["Q"]), vec![BuildIntent::EnterBuildMode(PieceKind::Stairs)], "remembered slot");
        let mut bi2 = bi.clone(); press(&mut bi2, &b, &["Q"]);
        bi2.update(&b, |c| c == Key::from_name("Q").unwrap().poll_code().unwrap(), true, 0.); assert_eq!(bi2.building_slot(), 0, "Reset Building Choice -> slot 0");
    }
    #[test] fn f_keys_enter_build_mode_and_empty_trap_slot_is_ignored() {
        let (mut bi, b) = setup();
        assert_eq!(press(&mut bi, &b, &["F2"]), vec![BuildIntent::EnterBuildMode(PieceKind::Floor)]);
        assert!(press(&mut bi, &b, &["F5"]).is_empty()); assert_eq!(bi.building_slot(), 1);
        bi.on_primary_slot_pressed(); assert_eq!(bi.drain_intents(), vec![BuildIntent::ExitBuildMode]);
    }
    #[test] fn material_cycles_only_while_holding_a_piece() {
        let (mut bi, b) = setup();
        assert!(press(&mut bi, &b, &["RightMouseButton"]).is_empty(), "no material change in combat");
        press(&mut bi, &b, &["Q"]);
        let seq: Vec<_> = (0..4).map(|_| press(&mut bi, &b, &["RightMouseButton"])).collect();
        assert_eq!(seq, vec![vec![BuildIntent::MaterialChanged(Material::Stone)], vec![BuildIntent::MaterialChanged(Material::Metal)], vec![BuildIntent::MaterialChanged(Material::Wood)], vec![BuildIntent::MaterialChanged(Material::Stone)]]);
    }
    #[test] fn holding_fire_places_once_no_turbo() {
        let (mut bi, b) = setup(); press(&mut bi, &b, &["Q"]);
        let lmb = Key::from_name("LeftMouseButton").unwrap().poll_code().unwrap();
        let mut n = 0; for _ in 0..120 { bi.update(&b, |c| c == lmb, false, 1. / 60.); n += bi.drain_intents().iter().filter(|i| **i == BuildIntent::Place).count(); }
        assert_eq!(n, 1);
    }
    #[test] fn edit_mode_routes_fire_reset_confirm() {
        let (mut bi, b) = setup(); press(&mut bi, &b, &["Q"]);
        assert_eq!(press(&mut bi, &b, &["G"]), vec![BuildIntent::EditBegin]);
        bi.set_editing(true);
        assert_eq!(press(&mut bi, &b, &["LeftMouseButton"]), vec![BuildIntent::EditSelectPressed, BuildIntent::EditSelectReleased]);
        assert_eq!(press(&mut bi, &b, &["RightMouseButton"]), vec![BuildIntent::EditReset], "reset, not a material change, while editing");
        assert_eq!(press(&mut bi, &b, &["G"]), vec![BuildIntent::EditConfirm]);
        assert_eq!(bi.material(), Material::Wood);
    }
    #[test] fn wheel_cycles_valid_building_slots() {
        // ConfigD_Athena (CookedIniVersion.txt L2665/L2666): SlotUp = MouseScrollDown, SlotDown = MouseScrollUp. SlotUp -> FUN_14083ce70 ->
        // FUN_140684240 steps +1 over the focused bar's valid slots and wraps; SlotDown (FUN_14083ce10) steps -1.
        let (mut bi, b) = setup(); press(&mut bi, &b, &["F4"]);
        assert_eq!(press(&mut bi, &b, &["MouseScrollUp"]), vec![BuildIntent::SelectPiece(PieceKind::Stairs)], "wheel up = SlotDown: Roof -> Stairs");
        let (mut bi, b) = setup(); press(&mut bi, &b, &["F4"]);
        assert_eq!(press(&mut bi, &b, &["MouseScrollDown"]), vec![BuildIntent::SelectPiece(PieceKind::Wall)], "wheel down = SlotUp: skips the empty trap slot and wraps to Wall");
    }
    /// Hold Fire with the pickaxe (the combo blocks InteractBuild) and press G: the press is refused (Building.Edit.Failed), editing never starts and the press
    /// is not buffered; once the block is gone a NEW press begins the edit (vfunc_669 @14083e780).
    #[test] fn edit_press_is_refused_while_interact_build_is_blocked_and_not_buffered() {
        let (mut bi, b) = setup();
        let (lmb, g) = (Key::from_name("LeftMouseButton").unwrap().poll_code().unwrap(), Key::from_name("G").unwrap().poll_code().unwrap());
        bi.update(&b, |c| c == lmb, false, 1. / 60.); assert!(bi.drain_intents().is_empty(), "Fire in combat is the pickaxe's, not a build intent");
        bi.set_interact_build_blocked(true);
        bi.update(&b, |c| c == lmb || c == g, false, 1. / 60.);
        let intents = bi.drain_intents(); assert_eq!(intents, vec![BuildIntent::EditBegin]);
        assert!(!bi.try_interact_build()); assert_eq!(bi.edit_failed_count(), 1); assert!(!bi.is_editing());
        // still holding G after the swing ends: no new edge, nothing begins
        bi.set_interact_build_blocked(false);
        bi.update(&b, |c| c == g, false, 1. / 60.); assert!(bi.drain_intents().is_empty(), "the refused press is not buffered");
        bi.update(&b, |_| false, false, 1. / 60.);
        bi.update(&b, |c| c == g, false, 1. / 60.); assert_eq!(bi.drain_intents(), vec![BuildIntent::EditBegin]);
        assert!(bi.try_interact_build()); assert_eq!(bi.edit_failed_count(), 1);
    }
    #[test] fn quickbar_switch_during_edit_cancels_then_switches() {
        let (mut bi, b) = setup(); press(&mut bi, &b, &["Q"]);
        bi.set_editing(true);
        assert_eq!(press(&mut bi, &b, &["Q"]), vec![BuildIntent::EditCancel, BuildIntent::ExitBuildMode]);
        assert!(!bi.is_editing() && !bi.is_building());
        let (mut bi, b) = setup(); bi.set_editing(true);
        assert_eq!(press(&mut bi, &b, &["Q"]), vec![BuildIntent::EditCancel, BuildIntent::EnterBuildMode(PieceKind::Wall)], "edit from combat: Q cancels and focuses the building bar");
    }
    #[test] fn slot_keys_and_wheel_during_edit_cancel() {
        let (mut bi, b) = setup(); press(&mut bi, &b, &["Q"]); bi.set_editing(true);
        bi.on_primary_slot_pressed();
        assert_eq!(bi.drain_intents(), vec![BuildIntent::EditCancel, BuildIntent::ExitBuildMode]);
        let (mut bi, b) = setup(); bi.set_editing(true);
        bi.on_primary_slot_pressed();
        assert_eq!(bi.drain_intents(), vec![BuildIntent::EditCancel]);
        let (mut bi, b2) = setup(); press(&mut bi, &b2, &["Q"]); bi.set_editing(true);
        assert_eq!(press(&mut bi, &b2, &["F1"]), vec![BuildIntent::EditCancel], "same building slot: the build tool comes back, the edit is cancelled");
        let (mut bi, b2) = setup(); press(&mut bi, &b2, &["Q"]); bi.set_editing(true);
        assert_eq!(press(&mut bi, &b2, &["F2"]), vec![BuildIntent::EditCancel, BuildIntent::SelectPiece(PieceKind::Floor)]);
        let (mut bi, b2) = setup(); press(&mut bi, &b2, &["Q"]); bi.set_editing(true);
        assert!(press(&mut bi, &b2, &["F5"]).is_empty(), "the empty trap slot changes nothing");
        assert!(bi.is_editing());
        let (mut bi, b2) = setup(); press(&mut bi, &b2, &["Q"]); bi.set_editing(true);
        assert_eq!(press(&mut bi, &b2, &["MouseScrollDown"]), vec![BuildIntent::EditCancel, BuildIntent::SelectPiece(PieceKind::Floor)]);
        let _ = b;
    }
    #[test] fn edit_keys_still_route_to_the_session() {
        let (mut bi, b) = setup(); bi.set_editing(true);
        assert_eq!(press(&mut bi, &b, &["LeftMouseButton"]), vec![BuildIntent::EditSelectPressed, BuildIntent::EditSelectReleased]);
        assert!(bi.is_editing());
    }
    fn frame(bi: &mut BuildInput, b: &Bindings, keys: &[&str]) -> Vec<BuildIntent> {
        let down: Vec<u16> = keys.iter().map(|k| Key::from_name(k).unwrap().poll_code().unwrap()).collect();
        bi.update(b, |c| down.contains(&c), false, 1. / 60.);
        let i = bi.drain_intents();
        if i.contains(&BuildIntent::EditBegin) { bi.set_editing(true); }
        if i.contains(&BuildIntent::EditConfirm) { bi.set_editing(false); }
        i
    }
    #[test] fn confirm_needs_a_second_press() {
        let (mut bi, b) = setup(); frame(&mut bi, &b, &["Q"]); frame(&mut bi, &b, &[]);
        assert_eq!(frame(&mut bi, &b, &["G"]), vec![BuildIntent::EditBegin]);
        assert!(frame(&mut bi, &b, &["G"]).is_empty());
        assert!(frame(&mut bi, &b, &[]).is_empty(), "releasing G does not confirm");
        assert_eq!(frame(&mut bi, &b, &["G"]), vec![BuildIntent::EditConfirm]);
    }
    #[test] fn reset_happens_on_the_press() {
        let (mut bi, b) = setup(); frame(&mut bi, &b, &["Q"]); frame(&mut bi, &b, &[]); frame(&mut bi, &b, &["G"]); frame(&mut bi, &b, &[]);
        assert_eq!(frame(&mut bi, &b, &["RightMouseButton"]), vec![BuildIntent::EditReset], "off: the press resets");
        assert!(frame(&mut bi, &b, &[]).is_empty());
    }
    #[test] fn afford_message_expires() { let mut bi = BuildInput::new(); bi.unable_to_afford(Material::Metal); assert_eq!(bi.afford_message(), Some(Material::Metal)); bi.update(&Settings::default().input.bindings, |_| false, false, 5.); assert_eq!(bi.afford_message(), None); }
}

/// BUILD INPUT -> EDIT HOOK binding (for the app side that owns the `BuildingWorld` and the `EditHook`): runs this frame's edit intents through the
/// editing session in the order the keys were handled and keeps `BuildInput::is_editing` in sync with the session.
/// `target` = the building under the crosshair (`PerformBuildingEditInteraction` acts on the focused building), `pawn` = pawn location (cm).
pub fn apply_edit_intents(input: &mut BuildInput, intents: &[BuildIntent], hook: &mut crate::building_edit_hook::EditHook, world: &mut fn_building::world::BuildingWorld,
    target: Option<fn_building::world::PieceId>, pawn: glam::Vec3) {
    for i in intents {
        match i {
            // the controller-level InteractBuild gate (`try_interact_build`, vfunc_669) runs before ServerBeginEditingBuildingActor and its per-building checks
            BuildIntent::EditBegin => { if input.try_interact_build() { let _ = hook.edit_begin(world, target, pawn); } }
            BuildIntent::EditSelectPressed => hook.edit_press(),
            BuildIntent::EditSelectReleased => hook.edit_release(),
            BuildIntent::EditReset => hook.edit_reset(),
            BuildIntent::EditConfirm => { hook.edit_confirm(world); }
            // leaving build mode / switching weapons unequips the edit tool: 1.8 cancels an unconfirmed edit (AFortWeap_EditingTool::OnUnEquip)
            BuildIntent::ExitBuildMode | BuildIntent::EditCancel => hook.edit_cancel(),
            _ => {}
        }
        input.set_editing(hook.is_editing());
    }
    input.set_editing(hook.is_editing());
}

#[cfg(test)]
mod default_key_tests {
    use super::*;
    /// ConfigD_Athena (CookedIniVersion.txt L2627-2761) through the generated binding table; a cleared row stays unbound, a player key wins.
    #[test] fn defaults_are_config_d_athena_from_the_table_and_player_keys_win() {
        let mut b = fn_settings::Settings::default().input.bindings.clone();
        let name = |b: &Bindings, a: usize, s: usize| keys_for(b, a)[s].name();
        assert_eq!((name(&b, A_SWITCH_QUICKBAR, 0), name(&b, A_SWITCH_QUICKBAR, 1)), ("Q", "ThumbMouseButton"));
        for i in 0..5 { assert_eq!(name(&b, A_SECONDARY_QUICKBAR_SLOT_F1 + i, 0), format!("F{}", i + 1)); }
        assert_eq!(name(&b, A_CHANGE_MATERIAL, 0), "RightMouseButton"); assert_eq!(name(&b, A_BUILDING_EDIT_RESET, 0), "RightMouseButton");
        assert_eq!(name(&b, A_PERFORM_BUILDING_EDIT_INTERACTION, 0), "G"); assert_eq!(name(&b, A_ROTATE, 0), "R");
        assert_eq!((name(&b, A_PERFORM_BUILDING_IMPROVEMENT_INTERACTION, 0), name(&b, A_PERFORM_BUILDING_IMPROVEMENT_INTERACTION, 1)), ("F", "ThumbMouseButton2"));
        assert_eq!((name(&b, A_SLOT_UP, 0), name(&b, A_SLOT_DOWN, 0)), ("MouseScrollDown", "MouseScrollUp"));
        b.keys[A_CHANGE_MATERIAL] = [fn_settings::Key::NONE; 2];
        assert_eq!(name(&b, A_CHANGE_MATERIAL, 0), "None");
        b.keys[A_CHANGE_MATERIAL][0] = fn_settings::Key::from_name("X").unwrap();
        assert_eq!(name(&b, A_CHANGE_MATERIAL, 0), "X");
    }
}
