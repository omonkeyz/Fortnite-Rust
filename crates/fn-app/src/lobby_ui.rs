//! BR lobby UI layer (1.8 `FrontEnd` -> `AthenaTabsScreen` -> `AthenaLobby` UMG data) for the lobby binary.
//!
//! `LobbyUi::load(install)` decodes the widget blueprints from the paks; `LobbyUi::draw(w, h, &state)` lays the screen out for a pixel viewport and returns a
//! transparent [`RenderList`] (composite it over the 3D scene) plus the element rectangles. Textures / baked materials live in `ctx().assets`: hand
//! `ctx_mut().assets` to the UI renderer together with the list. Everything here is read from the cooked 1.8 data; see `parity/lobby/UI.md`.
//! Interaction (hover / press / click through the UMG hit test, tab switching, the Locker) is `fn_ui::screens::athena_locker::controller::Controller`; see `parity/lobby/LOCKER.md`.
use fn_ui::draw::RenderList;
use fn_ui::layout::Rect;
use fn_ui::scene::UiContext;
pub use fn_ui::screens::athena_lobby::{paint::BlurRegion, Frame, LobbyState, Pad, PadKind};
pub use fn_ui::screens::athena_locker::controller::{Controller, Role, UiEvent};
pub use fn_ui::screens::athena_locker::{LockerItem, LockerView, Slot as LockerSlot};
use fn_ui::screens::athena_lobby::AthenaLobbyUi;
use std::path::Path;

pub struct LobbyUi { ui: AthenaLobbyUi, ctx: UiContext }

/// What a pointer position is over (for hover / click handling by the caller).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit { Tab(usize), Play, PlaylistRotator, TeamFillRotator, MainMenuButton, None }

impl LobbyUi {
    pub fn load(install: &Path) -> Result<LobbyUi, String> {
        let ctx = UiContext::from_install(install, None)?;
        let ui = AthenaLobbyUi::load(install)?;
        Ok(LobbyUi { ui, ctx })
    }
    pub fn load_context(install: &Path) -> Result<UiContext, String> { UiContext::from_install(install, None) }
    pub fn with_context(ctx: UiContext, install: &Path) -> Result<LobbyUi, String> { Ok(LobbyUi { ui: AthenaLobbyUi::load(install)?, ctx }) }
    pub fn ctx_mut(&mut self) -> &mut UiContext { &mut self.ctx }
    pub fn locres(&self) -> Option<std::rc::Rc<fn_ui::screens::athena_lobby::store::Locres>> { self.ui.ui.store.locres() }
    pub fn tab_labels(&self) -> &[String] { self.ui.tab_labels() }
    /// The interpreter-level UI (tree, roles, Locker widgets) for the `Controller`.
    pub fn inner(&self) -> &AthenaLobbyUi { &self.ui }
    pub fn inner_mut(&mut self) -> &mut AthenaLobbyUi { &mut self.ui }
    /// Index of the visible tab whose content is `class` (e.g. `athena_locker::TAB_CLASS`).
    pub fn tab_index_of(&self, class: &str) -> Option<usize> { (0..self.ui.tab_labels().len()).find(|&i| self.ui.tab_content(i) == Some(class)) }
    /// Lay out and paint for a `w` x `h` pixel viewport. Nothing is drawn behind the widgets.
    pub fn draw(&mut self, w: f32, h: f32, state: &LobbyState) -> Frame { self.ui.draw(&mut self.ctx, w, h, state) }
    /// Draw list only.
    pub fn draw_list(&mut self, w: f32, h: f32, state: &LobbyState) -> RenderList { self.draw(w, h, state).list }
    pub fn hit(frame: &Frame, x: f32, y: f32) -> Hit {
        if let Some(i) = frame.tabs.iter().position(|(_, r)| r.contains(x, y)) { return Hit::Tab(i); }
        let over = |n: &str| frame.rect(n).map_or(false, |r: Rect| r.contains(x, y));
        if over("PlayButton") { Hit::Play } else if over("PlaylistRotator") { Hit::PlaylistRotator } else if over("TeamFillRotator") { Hit::TeamFillRotator } else if over("MainMenuButton") { Hit::MainMenuButton } else { Hit::None }
    }
}
