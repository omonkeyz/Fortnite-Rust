use crate::lobby_ui::{Frame, LobbyState, LobbyUi};
use fn_ui::screens::athena_lobby::{ErrorDialogView, FinderButton, InviteRow, PartyFinderView};

const CHAT_MAX: usize = 120;
const JOIN_MAX: usize = 6;

#[derive(Default)]
pub struct PartyUi {
    pub finder: Option<PartyFinderView>,
    invites: Vec<(String, InviteRow)>,
    pub chat_input: Option<String>,
    pub join_input: Option<String>,
    pub error: Option<ErrorDialogView>,
    pub hint: Option<String>,
    announced: String,
    logged: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PartyAction { None, Join(String), Close }

impl PartyUi {
    pub fn modal_open(&self) -> bool { self.finder.is_some() || self.error.is_some() }

    pub fn poll(&mut self, party: Option<&crate::mp_hook::PartyView>) {
        for p in crate::mp_hook::take_peeks() {
            let row = InviteRow { name: if p.reached && !p.leader.is_empty() { p.leader.clone() } else { p.code.clone() }, joinable: !p.reached || (p.open && p.size < p.max.max(1)) };
            match self.invites.iter_mut().find(|(c, _)| *c == p.code) { Some(r) => r.1 = row, None => self.invites.push((p.code.clone(), row)) }
        }
        if let Some(code) = crate::mp_hook::take_party_error() { self.hint = Some(error_text(&code).to_owned()); }
        let code = party.filter(|p| p.me.is_some()).map(|p| p.code.clone()).unwrap_or_default();
        if code != self.announced {
            self.announced = code.clone();
            self.hint = if code.is_empty() { None } else { Some(format!("Party code: {code}")) };
        }
        if let Some(f) = self.finder.as_mut() {
            f.invites = self.invites.iter().map(|(_, r)| r.clone()).collect();
            f.selected = f.selected.min(f.rows() - 1);
        }
    }

    pub fn apply(&self, st: &mut LobbyState) {
        st.party_finder = self.finder.clone();
        st.error_dialog = self.error.clone();
        st.chat_input = self.chat_input.clone();
        st.join_code_input = self.join_input.clone();
    }

    pub fn open_finder(&mut self) {
        self.finder = Some(PartyFinderView { invites: self.invites.iter().map(|(_, r)| r.clone()).collect(), friends: 0, selected: 0, hovered: None, hovered_button: None, pressed_button: None });
    }

    pub fn show_error(&mut self, message: String) { self.error = Some(ErrorDialogView { message, close_hovered: false }); }

    fn hits(ui: &LobbyUi, frame: &Frame, rows: usize) -> (Vec<fn_ui::layout::Rect>, [Option<fn_ui::layout::Rect>; 3], Option<fn_ui::layout::Rect>) {
        let (r, b, e) = ui.inner().social_hit_ids(rows);
        let at = |id: usize| frame.id_rects.get(id).copied();
        (r.into_iter().filter_map(at).collect(), b.map(|x| x.and_then(at)), e.and_then(at))
    }

    pub fn log_layout(&mut self, ui: &LobbyUi, frame: &Frame, blur_ok: bool) {
        let key = self.finder.as_ref().map(|f| f.rows() * 100 + f.selected * 2 + 1).unwrap_or(0) * 10 + self.error.is_some() as usize;
        if key == self.logged { return; }
        self.logged = key;
        let fmt = |r: Option<fn_ui::layout::Rect>| r.map_or("-".to_owned(), |r| format!("{:.0},{:.0},{:.0},{:.0}", r.x, r.y, r.w, r.h));
        let (rows, b, e) = Self::hits(ui, frame, self.finder.as_ref().map_or(0, |f| f.rows()));
        eprintln!("[lobby] social layout rows=[{}] join={} ignore={} close={} error_close={} blur={blur_ok}", rows.iter().map(|r| fmt(Some(*r))).collect::<Vec<_>>().join(" "), fmt(b[0]), fmt(b[1]), fmt(b[2]), fmt(e));
    }

    pub fn pointer_move(&mut self, ui: &LobbyUi, frame: &Frame, x: f32, y: f32) {
        if let Some(e) = self.error.as_mut() {
            let (_, _, c) = Self::hits(ui, frame, 0);
            e.close_hovered = c.is_some_and(|r| r.contains(x, y));
            return;
        }
        let Some(f) = self.finder.as_mut() else { return };
        let (rows, buttons, _) = Self::hits(ui, frame, f.rows());
        f.hovered = rows.iter().position(|r| r.contains(x, y));
        f.hovered_button = [FinderButton::Join, FinderButton::Ignore, FinderButton::Close].into_iter().zip(buttons).find(|(_, r)| r.is_some_and(|r| r.w > 0.0 && r.contains(x, y))).map(|(b, _)| b);
    }

    pub fn pointer_button(&mut self, ui: &LobbyUi, frame: &Frame, x: f32, y: f32, down: bool) -> PartyAction {
        if self.error.is_some() {
            let (_, _, c) = Self::hits(ui, frame, 0);
            if !down && c.is_some_and(|r| r.contains(x, y)) { self.error = None; }
            return PartyAction::None;
        }
        let Some(f) = self.finder.as_mut() else { return PartyAction::None };
        let (rows, buttons, _) = Self::hits(ui, frame, f.rows());
        let over = [FinderButton::Join, FinderButton::Ignore, FinderButton::Close].into_iter().zip(buttons).find(|(_, r)| r.is_some_and(|r| r.w > 0.0 && r.contains(x, y))).map(|(b, _)| b);
        if down { f.pressed_button = over; return PartyAction::None; }
        let pressed = f.pressed_button.take();
        if let Some(i) = rows.iter().position(|r| r.contains(x, y)) { f.selected = i; return PartyAction::None; }
        match (over, pressed) {
            (Some(FinderButton::Close), Some(FinderButton::Close)) => { self.finder = None; PartyAction::Close }
            (Some(FinderButton::Ignore), Some(FinderButton::Ignore)) => {
                if f.invite_at(f.selected).is_some() { let i = f.selected - 1; self.invites.remove(i); f.invites.remove(i); f.selected = 0; }
                PartyAction::None
            }
            (Some(FinderButton::Join), Some(FinderButton::Join)) => {
                let Some(inv) = f.invite_at(f.selected).cloned() else { return PartyAction::None };
                if !inv.joinable { return PartyAction::None; }
                let code = self.invites[f.selected - 1].0.clone();
                self.invites.remove(f.selected - 1);
                self.finder = None;
                PartyAction::Join(code)
            }
            _ => PartyAction::None,
        }
    }

    pub fn escape(&mut self) -> bool {
        if self.chat_input.take().is_some() { return true; }
        if self.join_input.take().is_some() { return true; }
        if self.error.take().is_some() { return true; }
        if self.finder.take().is_some() { return true; }
        false
    }

    pub fn open_chat(&mut self) { self.join_input = None; if self.chat_input.is_none() { self.chat_input = Some(String::new()); } }

    pub fn open_join(&mut self) { self.chat_input = None; if self.join_input.is_none() { self.join_input = Some(String::new()); } }

    pub fn join_key(&mut self, k: fn_console::Key) -> PartyAction {
        let Some(buf) = self.join_input.as_mut() else { return PartyAction::None };
        match k {
            fn_console::Key::Char(c) => { if !c.is_whitespace() && !c.is_control() && buf.chars().count() < JOIN_MAX { buf.push(c.to_ascii_uppercase()); } }
            fn_console::Key::Backspace => { buf.pop(); }
            fn_console::Key::Escape => { self.join_input = None; }
            fn_console::Key::Enter => {
                let text = self.join_input.take().unwrap_or_default();
                if text.trim().is_empty() { return PartyAction::None; }
                let Some(code) = crate::chat_hook::valid_code(&text) else { self.hint = Some(error_text("no_lobby").to_owned()); return PartyAction::None };
                if code.eq_ignore_ascii_case(&self.announced) { return PartyAction::None; }
                return PartyAction::Join(code);
            }
            _ => {}
        }
        PartyAction::None
    }

    pub fn chat_key(&mut self, k: fn_console::Key) {
        let Some(buf) = self.chat_input.as_mut() else { return };
        match k {
            fn_console::Key::Char(c) => { if buf.chars().count() < CHAT_MAX { buf.push(c); } }
            fn_console::Key::Backspace => { buf.pop(); }
            fn_console::Key::Escape => { self.chat_input = None; }
            fn_console::Key::Enter => {
                let text = self.chat_input.take().unwrap_or_default();
                self.submit(text.trim());
            }
            _ => {}
        }
    }

    fn submit(&mut self, text: &str) {
        if text.is_empty() { return; }
        let code = text.strip_prefix("/join").map(str::trim).unwrap_or(text);
        if let Some(code) = crate::chat_hook::valid_code(code) {
            if !self.invites.iter().any(|(c, _)| *c == code) { self.invites.push((code.clone(), InviteRow { name: code.clone(), joinable: true })); }
            crate::mp_hook::party_peek(&code);
            let row = self.invites.iter().position(|(c, _)| *c == code).map_or(0, |i| i + 1);
            self.open_finder();
            if let Some(f) = self.finder.as_mut() { f.selected = row; }
            return;
        }
        crate::chat_hook::submit(text);
    }

    pub fn chat_strip_hit(w: f32, h: f32, x: f32, y: f32) -> bool {
        let scale = fn_ui::layout::dpi_scale(w, h);
        y >= h - 58.0 * scale && x < w * 0.5
    }
}

fn error_text(code: &str) -> &'static str {
    match code {
        "no_lobby" => "No party with that code.",
        "full" => "That party is full.",
        "bad_name" => "That name is not allowed.",
        "host_left" | "host_ended" => "The party leader left.",
        "kicked" | "host" => "You were removed from the party.",
        _ => "Could not reach the party service.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_typed_in_the_chat_bar_becomes_a_party_invite_row() {
        let mut p = PartyUi::default();
        p.open_chat();
        for c in "abc234".chars() { p.chat_key(fn_console::Key::Char(c)); }
        assert_eq!(p.chat_input.as_deref(), Some("abc234"));
        p.chat_key(fn_console::Key::Enter);
        assert!(p.chat_input.is_none());
        let f = p.finder.as_ref().unwrap();
        assert_eq!(f.rows(), 3);
        assert_eq!(f.invites, vec![InviteRow { name: "ABC234".into(), joinable: true }]);
        assert_eq!(f.selected, 1);
        assert!(f.invite_at(0).is_none() && f.invite_at(1).is_some() && f.invite_at(2).is_none());
    }

    #[test]
    fn the_join_code_box_joins_a_typed_code_and_ignores_bad_or_own_codes() {
        let mut p = PartyUi::default();
        p.open_join();
        for c in " ab c23 4x".chars() { assert_eq!(p.join_key(fn_console::Key::Char(c)), PartyAction::None); }
        assert_eq!(p.join_input.as_deref(), Some("ABC234"));
        assert_eq!(p.join_key(fn_console::Key::Enter), PartyAction::Join("ABC234".into()));
        assert!(p.join_input.is_none() && p.finder.is_none());
        p.open_join();
        for c in "abc".chars() { p.join_key(fn_console::Key::Char(c)); }
        assert_eq!(p.join_key(fn_console::Key::Enter), PartyAction::None);
        assert_eq!(p.hint.as_deref(), Some("No party with that code."));
        p.announced = "ZZZ234".into();
        p.open_join();
        for c in "zzz234".chars() { p.join_key(fn_console::Key::Char(c)); }
        assert_eq!(p.join_key(fn_console::Key::Enter), PartyAction::None);
        p.open_join();
        p.join_key(fn_console::Key::Char('q'));
        assert!(p.escape() && p.join_input.is_none());
        p.open_join();
        p.open_chat();
        assert!(p.join_input.is_none() && p.chat_input.is_some());
    }

    #[test]
    fn escape_closes_the_chat_input_then_the_error_then_the_party_finder() {
        let mut p = PartyUi::default();
        p.open_finder();
        p.show_error("Party is too big for this game mode".into());
        p.open_chat();
        assert!(p.escape() && p.chat_input.is_none() && p.error.is_some());
        assert!(p.escape() && p.error.is_none() && p.finder.is_some());
        assert!(p.escape() && p.finder.is_none());
        assert!(!p.escape());
    }
}
