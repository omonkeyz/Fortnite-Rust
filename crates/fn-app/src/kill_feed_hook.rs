use fn_ui::screens::kill_feed::{Combatant, DeathCause, Elimination, KillFeed, Killer, CHEAT_MESSAGES};
use std::sync::Mutex;

struct State { feed: KillFeed, now: f32, local_name: String, alive: bool, kills: u32, zero_vitals: bool, elims: u32, death: Option<(DeathCause, Killer)>, won: bool }
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner());
    let st = g.get_or_insert_with(|| State { feed: KillFeed::new(), now: 0., local_name: String::new(), alive: true, kills: 0, zero_vitals: false, elims: 0, death: None, won: false });
    f(st)
}

pub fn set_local_name(name: &str) { with(|s| if s.local_name != name { s.local_name = name.to_owned(); }); }

pub fn local() -> Combatant { with(|s| Combatant::local(if s.local_name.is_empty() { "Player" } else { &s.local_name })) }

pub fn push(e: Elimination) {
    with(|s| {
        let now = s.now;
        if !e.dbno { s.elims += 1; }
        if let Some(m) = s.feed.push(&e, now) {
            if e.counts_for_local() { s.kills += 1; }
            eprintln!("[killfeed] {m}");
        }
    });
}

pub fn push_markup(markup: &str) { with(|s| { let now = s.now; s.feed.push_markup(markup.to_owned(), now); eprintln!("[killfeed] {markup}"); }); }

pub fn add_cheat_messages() { for m in CHEAT_MESSAGES { push_markup(m); } }

pub fn local_damaged(cause: DeathCause, killer: Killer, health_after: f32) {
    let died = with(|s| { if s.alive && health_after <= 0. { s.alive = false; s.death = Some((cause, killer.clone())); true } else { false } });
    if died { push(Elimination { killer, victim: local(), cause, dbno: false }); }
}

pub fn note_local_death(cause: DeathCause, killer: Killer) { with(|s| { s.alive = false; if s.death.is_none() { s.death = Some((cause, killer)); } }); }

pub fn force_local_death(cause: DeathCause) -> Option<String> {
    with(|s| { s.alive = false; s.zero_vitals = true; s.death = Some((cause, Killer::Themself)); });
    let e = Elimination { killer: Killer::Themself, victim: local(), cause, dbno: false };
    let m = e.markup();
    push(e);
    m
}

pub fn frame(world_time: f32, local_health: Option<f32>) -> Vec<String> {
    with(|s| {
        s.now = world_time;
        if local_health.map_or(false, |h| h > 0.) { s.alive = true; s.death = None; }
        s.feed.tick(world_time);
        s.feed.lines()
    })
}

pub fn kills() -> u32 { with(|s| s.kills) }

pub fn eliminations() -> u32 { with(|s| s.elims) }

pub fn reset_eliminations() { with(|s| s.elims = 0); }

pub fn take_zero_vitals() -> bool { with(|s| std::mem::take(&mut s.zero_vitals)) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn a_fall_to_zero_pushes_one_line_until_health_returns() {
        set_local_name("Tester");
        frame(0., Some(100.));
        local_damaged(DeathCause::FallDamage, Killer::Themself, 0.);
        local_damaged(DeathCause::FallDamage, Killer::Themself, 0.);
        let l = frame(1., Some(0.));
        assert_eq!(l.iter().filter(|m| m.contains("didn't stick the landing")).count(), 1);
        frame(2., Some(100.));
        local_damaged(DeathCause::Heavy, Killer::Themself, 0.);
        assert!(frame(3., Some(0.)).iter().any(|m| m == "<TeamVictim>Tester</> checked out early"));
        assert!(frame(20., Some(0.)).is_empty());
    }
}

pub fn local_death() -> Option<(DeathCause, Killer)> { with(|s| s.death.clone()) }

pub fn local_name() -> String { with(|s| if s.local_name.is_empty() { "Player".to_owned() } else { s.local_name.clone() }) }

pub fn set_won(won: bool) { with(|s| s.won = won); }

pub fn won() -> bool { with(|s| s.won) }
