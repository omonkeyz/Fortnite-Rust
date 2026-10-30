//! The line editor of the command bar: text input with a caret, scrollback, command history (Up / Down), Tab completion with cycling, open / close state.
//! Key events are abstract ([`Key`]); the front-end maps its window events to them, so this is testable without a window.
use crate::catalog::Catalog;
use crate::complete::{complete, Completion, Suggestion};
use crate::exec::{execute, CosmeticSource, Effects, Env, Flags, LineKind};
use crate::registry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key { Char(char), Backspace, Delete, Left, Right, Home, End, Up, Down, PageUp, PageDown, Tab, ShiftTab, Enter, Escape, ToggleConsole }

pub const LOG_CAP: usize = 500;
const PAGE: usize = 6;

#[derive(Default)]
struct TabCycle { token_start: usize, items: Vec<Suggestion>, idx: usize }

/// Everything the UI needs to draw one frame of the bar.
pub struct View<'a> {
    pub input: &'a str,
    /// caret position in characters
    pub cursor: usize,
    pub log: &'a [(LineKind, String)],
    /// log lines hidden below the shown window (the user scrolled up)
    pub scrolled: usize,
    pub items: &'a [Suggestion],
    pub hint: &'a str,
    /// the suggestion highlighted by Tab cycling
    pub selected: Option<usize>,
}

#[derive(Default)]
pub struct Console {
    open: bool,
    input: String,
    cursor: usize,
    history: Vec<String>,
    hist_pos: Option<usize>,
    draft: String,
    log: Vec<(LineKind, String)>,
    scroll: usize,
    cycle: Option<TabCycle>,
    selected: Option<usize>,
    comp: Completion,
    /// god / infinite ammo: the front-end applies them every frame.
    pub flags: Flags,
}

impl Console {
    pub fn new() -> Console {
        let mut c = Console::default();
        c.log.push((LineKind::Info, "Command bar (~ / Esc closes). help lists the commands; Tab completes; Up / Down = history.".into()));
        c
    }
    pub fn is_open(&self) -> bool { self.open }
    pub fn open(&mut self) { self.open = true; }
    pub fn close(&mut self) { self.open = false; self.cycle = None; self.selected = None; }
    pub fn toggle(&mut self) { if self.open { self.close() } else { self.open() } }
    pub fn input(&self) -> &str { &self.input }
    pub fn cursor(&self) -> usize { self.cursor }
    pub fn log(&self) -> &[(LineKind, String)] { &self.log }
    pub fn history(&self) -> &[String] { &self.history }
    pub fn completion(&self) -> &Completion { &self.comp }
    /// Appends a line (also used by the front-end to report what applying a request did).
    pub fn say(&mut self, k: LineKind, s: impl Into<String>) { self.log.push((k, s.into())); self.scroll = 0; let n = self.log.len(); if n > LOG_CAP { self.log.drain(0..n - LOG_CAP); } }
    pub fn clear_log(&mut self) { self.log.clear(); self.scroll = 0; }
    pub fn set_input(&mut self, s: &str) { self.input = s.to_owned(); self.cursor = s.chars().count(); self.cycle = None; self.selected = None; }
    /// Recompute the suggestions for the current input (call after `open`, the catalog changing, ...; `key` does it itself).
    pub fn refresh(&mut self, cat: &Catalog, cs: &dyn CosmeticSource) { self.comp = complete(&self.input, cat, cs); }

    /// The state to draw. `lines` = how many log lines fit above the input row.
    pub fn view(&self, lines: usize) -> View<'_> {
        let end = self.log.len().saturating_sub(self.scroll.min(self.log.len()));
        let start = end.saturating_sub(lines);
        View { input: &self.input, cursor: self.cursor, log: &self.log[start..end], scrolled: self.log.len() - end, items: &self.comp.items, hint: &self.comp.hint, selected: self.selected }
    }
    pub fn scroll_by(&mut self, lines: i32) { self.scroll = (self.scroll as i64 + lines as i64).clamp(0, self.log.len() as i64) as usize; }

    fn byte_at(&self, chars: usize) -> usize { self.input.char_indices().nth(chars).map(|(i, _)| i).unwrap_or(self.input.len()) }
    fn edited(&mut self) { self.cycle = None; self.selected = None; self.hist_pos = None; }

    /// One key while the bar is open. Returns the effects of an executed command (the front-end applies `requests`); the log lines are already appended.
    pub fn key(&mut self, k: Key, env: &mut Env) -> Effects {
        let mut fx = Effects::default();
        match k {
            Key::ToggleConsole | Key::Escape => self.close(),
            Key::Char(c) if (' '..='~').contains(&c) => { let at = self.byte_at(self.cursor); self.input.insert(at, c); self.cursor += 1; self.edited(); }
            Key::Char(_) => {}
            Key::Backspace => if self.cursor > 0 { let a = self.byte_at(self.cursor - 1); let b = self.byte_at(self.cursor); self.input.replace_range(a..b, ""); self.cursor -= 1; self.edited(); },
            Key::Delete => if self.cursor < self.input.chars().count() { let a = self.byte_at(self.cursor); let b = self.byte_at(self.cursor + 1); self.input.replace_range(a..b, ""); self.edited(); },
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.input.chars().count()),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = self.input.chars().count(),
            Key::PageUp => self.scroll_by(PAGE as i32),
            Key::PageDown => self.scroll_by(-(PAGE as i32)),
            Key::Up => {
                if self.history.is_empty() { return fx; }
                let pos = match self.hist_pos { None => { self.draft = self.input.clone(); self.history.len() - 1 } Some(p) => p.saturating_sub(1) };
                self.hist_pos = Some(pos); self.input = self.history[pos].clone(); self.cursor = self.input.chars().count(); self.cycle = None; self.selected = None;
            }
            Key::Down => {
                let Some(p) = self.hist_pos else { return fx };
                if p + 1 < self.history.len() { self.hist_pos = Some(p + 1); self.input = self.history[p + 1].clone(); } else { self.hist_pos = None; self.input = std::mem::take(&mut self.draft); }
                self.cursor = self.input.chars().count(); self.cycle = None; self.selected = None;
            }
            Key::Tab | Key::ShiftTab => { self.tab(k == Key::ShiftTab, env.catalog, env.cosmetics); }
            Key::Enter => {
                let line = self.input.trim().to_owned();
                if line.is_empty() { return fx; }
                if self.history.last() != Some(&line) { self.history.push(line.clone()); }
                self.input.clear(); self.cursor = 0; self.hist_pos = None; self.cycle = None; self.selected = None;
                self.say(LineKind::Input, format!("> {line}"));
                fx = execute(&line, env, &mut self.flags);
                if fx.clear { self.clear_log(); }
                for (kind, l) in &fx.lines { self.say(*kind, l.clone()); }
            }
        }
        if self.open { self.refresh(env.catalog, env.cosmetics); }
        fx
    }

    /// Tab: complete the token under the caret. One match = insert it (a command that takes arguments gets a trailing space); several = insert their common prefix first,
    /// then cycle through them on each further Tab (Shift+Tab backwards).
    fn tab(&mut self, back: bool, cat: &Catalog, cs: &dyn CosmeticSource) {
        if let Some(c) = &mut self.cycle {
            let n = c.items.len();
            if n > 0 {
                c.idx = if back { (c.idx + n - 1) % n } else { (c.idx + 1) % n };
                let (ins, start) = (c.items[c.idx].insert.clone(), c.token_start);
                self.input.replace_range(start.., &ins); self.cursor = self.input.chars().count(); self.selected = Some(c.idx);
            }
            return;
        }
        let comp = complete(&self.input, cat, cs);
        if comp.items.is_empty() { return; }
        let start = comp.token_start.min(self.input.len());
        let typed = self.input[start..].to_owned();
        let in_command = !self.input.trim_start().contains(char::is_whitespace);
        if comp.items.len() == 1 {
            let ins = comp.items[0].insert.clone();
            let mut text = ins.clone();
            if in_command && registry::find(&ins).map_or(false, |c| !c.usage.is_empty()) { text.push(' '); }
            self.input.replace_range(start.., &text); self.cursor = self.input.chars().count(); self.selected = Some(0);
            return;
        }
        let mut common = comp.items[0].insert.clone();
        for s in &comp.items[1..] { let l = common.chars().zip(s.insert.chars()).take_while(|(a, b)| a.eq_ignore_ascii_case(b)).count(); common = common.chars().take(l).collect(); }
        if common.len() > typed.trim().len() && comp.items.iter().all(|s| s.insert.to_ascii_lowercase().starts_with(&common.to_ascii_lowercase())) {
            self.input.replace_range(start.., &common); self.cursor = self.input.chars().count(); self.selected = None;
            return;
        }
        let ins = comp.items[0].insert.clone();
        self.cycle = Some(TabCycle { token_start: start, items: comp.items.clone(), idx: 0 });
        self.input.replace_range(start.., &ins); self.cursor = self.input.chars().count(); self.selected = Some(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::test_catalog;
    use crate::exec::tests::FakeCosmetics;
    use fn_game::GameSim;

    struct Rig { sim: GameSim, cat: Catalog, con: Console }
    impl Rig {
        fn new() -> Rig { let mut r = Rig { sim: GameSim::new(), cat: test_catalog(), con: Console::new() }; r.con.open(); r }
        fn key(&mut self, k: Key) -> Effects { let mut env = Env { sim: &mut self.sim, catalog: &self.cat, vitals: Default::default(), cosmetics: &FakeCosmetics }; self.con.key(k, &mut env) }
        fn type_str(&mut self, s: &str) { for c in s.chars() { self.key(Key::Char(c)); } }
        fn line(&mut self, s: &str) -> Effects { self.type_str(s); self.key(Key::Enter) }
    }

    #[test] fn typing_editing_and_the_caret() {
        let mut r = Rig::new();
        r.type_str("helo"); r.key(Key::Left); r.key(Key::Char('l')); assert_eq!(r.con.input(), "hello"); assert_eq!(r.con.cursor(), 4);
        r.key(Key::Home); r.key(Key::Delete); assert_eq!(r.con.input(), "ello"); r.key(Key::End); r.key(Key::Backspace); assert_eq!(r.con.input(), "ell");
        r.key(Key::Char('\u{e9}')); assert_eq!(r.con.input(), "ell", "non printable ASCII is ignored");
    }
    #[test] fn enter_runs_the_command_logs_the_result_and_keeps_history() {
        let mut r = Rig::new();
        let fx = r.line("give bandage 3"); assert_eq!(fx.requests.len(), 1);
        let log: Vec<&str> = r.con.log().iter().map(|l| l.1.as_str()).collect(); assert!(log.contains(&"> give bandage 3")); assert!(log.last().unwrap().contains("Bandage"));
        r.line("bogus"); assert_eq!(r.con.log().last().unwrap().0, LineKind::Error);
        r.line("bogus"); assert_eq!(r.con.history(), ["give bandage 3", "bogus"], "a repeated line is stored once");
        assert!(r.con.input().is_empty());
        r.line("clear"); assert!(r.con.log().is_empty() || r.con.log().len() == 1, "{:?}", r.con.log()); // only the echo of nothing: clear empties, the echo was dropped with it
    }
    #[test] fn history_walks_up_and_down_and_restores_the_draft() {
        let mut r = Rig::new();
        r.line("pos"); r.line("heal"); r.type_str("draft");
        r.key(Key::Up); assert_eq!(r.con.input(), "heal"); r.key(Key::Up); assert_eq!(r.con.input(), "pos"); r.key(Key::Up); assert_eq!(r.con.input(), "pos", "stops at the oldest");
        r.key(Key::Down); assert_eq!(r.con.input(), "heal"); r.key(Key::Down); assert_eq!(r.con.input(), "draft"); r.key(Key::Down); assert_eq!(r.con.input(), "draft");
        assert_eq!(r.con.cursor(), 5);
    }
    #[test] fn tab_completes_commands_arguments_and_cycles() {
        let mut r = Rig::new();
        r.type_str("gi"); r.key(Key::Tab); assert_eq!(r.con.input(), "give", "the common prefix of give / giveathena* / giveweapon ...");
        r.key(Key::Tab); let first = r.con.input().to_owned(); r.key(Key::Tab); assert_ne!(r.con.input(), first, "cycles through the matches");
        r.key(Key::ShiftTab); assert_eq!(r.con.input(), first);
        let mut r = Rig::new();
        r.type_str("ski"); r.key(Key::Tab); assert_eq!(r.con.input(), "skin ", "a command with arguments gets a space");
        r.type_str("beta"); r.key(Key::Tab); assert_eq!(r.con.input(), "skin CID_002");
        let fx = r.key(Key::Enter); assert!(r.con.log().last().unwrap().1.contains("Beta Skin"), "{:?} {:?}", fx, r.con.log().last());
        let mut r = Rig::new();
        r.type_str("give pu"); r.key(Key::Tab); assert_eq!(r.con.input(), "give WID_Shotgun_Standard_Athena_C_Ore_T03");
    }
    #[test] fn suggestions_follow_the_input() {
        let mut r = Rig::new();
        r.type_str("skin al"); assert_eq!(r.con.view(5).items.len(), 2); assert_eq!(r.con.view(5).hint, "");
        r.key(Key::Backspace); r.key(Key::Backspace); r.key(Key::Backspace); assert_eq!(r.con.view(5).hint, "<id|name>", "{:?}", r.con.completion());
    }
    #[test] fn escape_and_f2_close_and_scrollback_pages() {
        let mut r = Rig::new(); assert!(r.con.is_open());
        r.key(Key::Escape); assert!(!r.con.is_open()); r.con.open(); r.key(Key::ToggleConsole); assert!(!r.con.is_open());
        r.con.open(); for i in 0..30 { r.con.say(LineKind::Info, format!("l{i}")); }
        let v = r.con.view(5); assert_eq!((v.log.len(), v.scrolled, v.log.last().unwrap().1.as_str()), (5, 0, "l29"));
        r.key(Key::PageUp); let v = r.con.view(5); assert_eq!((v.scrolled, v.log.last().unwrap().1.as_str()), (6, "l23"));
        r.key(Key::PageDown); assert_eq!(r.con.view(5).scrolled, 0); r.key(Key::PageDown); assert_eq!(r.con.view(5).scrolled, 0);
        for _ in 0..600 { r.con.say(LineKind::Info, "x"); } assert_eq!(r.con.log().len(), LOG_CAP);
    }
    #[test] fn commands_dispatch_through_the_bar_onto_the_sim() {
        let mut r = Rig::new();
        r.line("gravity 3"); assert_eq!(r.sim.tuning.gravity_scale, Some(3.));
        r.line("god"); assert!(r.con.flags.god);
        r.line("tp 5 0 5"); assert_eq!(r.sim.x, 5.);
    }
}
