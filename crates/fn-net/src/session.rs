use crate::interp::{Interp, Sample};
use crate::proto::{self, Build, Fire, HealthEntry, Hit, Msg, State, Storm};
use glam::Vec3;
use std::collections::BTreeMap;

pub const SEND_HZ: f32 = 20.;
pub const MAX_HEALTH: f32 = 100.;
pub const MAX_SHIELD: f32 = 100.;

#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Welcome { slot: u8, host: bool, code: String, team: Option<u8>, playlist: usize, matched: bool },
    Lobby(Vec<(u8, String, bool, Option<u8>)>),
    Start { seed: u32, playlist: Option<usize>, squad: bool },
    Host { slot: u8 },
    Left { slot: u8 },
    End { reason: String },
    Kicked { reason: String },
    Error { code: String },
}

pub fn parse_control(text: &str) -> Option<Control> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_owned();
    Some(match v.get("t")?.as_str()? {
        "welcome" => Control::Welcome { slot: v.get("slot")?.as_u64()? as u8, host: v.get("host")?.as_bool()?, code: s("code"), team: v.get("team").and_then(|x| x.as_u64()).map(|t| t as u8), playlist: v.get("playlist").and_then(|x| x.as_u64()).unwrap_or(0).min(2) as usize, matched: v.get("match").and_then(|x| x.as_bool()).unwrap_or(false) },
        "lobby" => Control::Lobby(v.get("players")?.as_array()?.iter().filter_map(|p| Some((p.get("slot")?.as_u64()? as u8, p.get("name")?.as_str()?.to_owned(), p.get("host")?.as_bool()?, p.get("team").and_then(|x| x.as_u64()).map(|t| t as u8)))).collect()),
        "start" => Control::Start { seed: v.get("seed").and_then(|x| x.as_u64()).unwrap_or(0) as u32, playlist: v.get("playlist").and_then(|x| x.as_u64()).map(|p| p.min(2) as usize), squad: v.get("squad").and_then(|x| x.as_bool()).unwrap_or(false) },
        "host" => Control::Host { slot: v.get("slot")?.as_u64()? as u8 },
        "left" => Control::Left { slot: v.get("slot")?.as_u64()? as u8 },
        "end" => Control::End { reason: s("reason") },
        "kicked" => Control::Kicked { reason: s("reason") },
        "error" => Control::Error { code: s("code") },
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Started { seed: u32 },
    Ended(String),
    Damaged { attacker: u8, amount: f32, shield_hit: bool, head: bool, dbno: bool },
    DamageShown { victim: u8, amount: f32, shield_hit: bool, head: bool, dbno: bool },
    Hurt { victim: u8 },
    OwnHealth { health: f32, shield: f32, dead: bool },
    Elim { victim: u8, killer: u8, weapon: u8, head: bool, flags: u8 },
    Knocked { victim: u8, by: u8, weapon: u8, head: bool },
    Revived { victim: u8, by: u8 },
    ReviveStarted { victim: u8, by: u8 },
    ReviveStopped { victim: u8, by: u8 },
    Respawn(Vec3),
    Correct(Vec3),
    RemoteFire { slot: u8, fire: Fire },
    Build { slot: u8, build: Build },
    Interact { slot: u8, kind: u8, id: u32 },
    Storm(Storm),
    Joined { slot: u8, name: String },
    Left { slot: u8, name: String },
    PickupSpawned { id: u32, owner: u8, flags: u8, count: u16, start: Vec3, end: Vec3, item: String },
    PickupGone { id: u32, taker: u8 },
    Emote { slot: u8, op: u8, item: u8 },
}

#[derive(Default)]
pub struct Remote { pub interp: Interp, pub name: String, pub health: f32, pub shield: f32, pub dead: bool, pub seen: bool, pub dbno: bool, pub reviver: Option<u8> }
impl Remote {
    pub fn sample(&self, now_ms: f64) -> Option<Sample> { self.interp.sample(now_ms) }
}

pub struct Session {
    pub slot: u8,
    pub host: bool,
    pub code: String,
    pub started: bool,
    pub ended: Option<String>,
    pub names: BTreeMap<u8, String>,
    pub remotes: BTreeMap<u8, Remote>,
    pub health: f32,
    pub shield: f32,
    pub dead: bool,
    pub team: u8,
    pub teams: BTreeMap<u8, u8>,
    pub playlist: usize,
    pub squad: bool,
    pub matched: bool,
    pub dbno: bool,
    pub reviver: Option<u8>,
    outbox: Vec<Vec<u8>>,
    events: Vec<Event>,
    seq: u32,
    fire_seq: u32,
    send_accum: f32,
    reported: (u8, u8),
    storm_in: f32,
}

fn to_u8(v: f32) -> u8 { v.round().clamp(0., 100.) as u8 }

impl Session {
    pub fn new(slot: u8, host: bool, code: String) -> Session {
        Session { slot, host, code, started: false, ended: None, names: BTreeMap::new(), remotes: BTreeMap::new(), health: MAX_HEALTH, shield: 0., dead: false, team: slot, teams: BTreeMap::new(), playlist: 0, squad: false, matched: false, dbno: false, reviver: None,
            outbox: vec![], events: vec![], seq: 0, fire_seq: 0, send_accum: 1., reported: (100, 0), storm_in: 0. }
    }
    pub fn take_outbox(&mut self) -> Vec<Vec<u8>> { std::mem::take(&mut self.outbox) }
    pub fn take_events(&mut self) -> Vec<Event> { std::mem::take(&mut self.events) }
    pub fn name_of(&self, slot: u8) -> String { if slot == proto::NO_KILLER { "The Storm".into() } else { self.names.get(&slot).cloned().unwrap_or_else(|| format!("Player {slot}")) } }

    pub fn control(&mut self, c: Control) {
        match c {
            Control::Welcome { slot, host, code, team, playlist, matched } => { self.slot = slot; self.host = host; self.code = code; self.team = team.unwrap_or(slot); self.playlist = playlist; self.matched = matched; self.teams.insert(slot, self.team); }
            Control::Host { slot } => { if slot == self.slot { self.host = true; } }
            Control::Lobby(list) => {
                for (slot, name, _, team) in &list {
                    if !self.names.contains_key(slot) && *slot != self.slot { self.events.push(Event::Joined { slot: *slot, name: name.clone() }); }
                    self.names.insert(*slot, name.clone());
                    self.teams.insert(*slot, team.unwrap_or(*slot));
                    if *slot == self.slot { self.team = team.unwrap_or(*slot); }
                }
                let live: Vec<u8> = list.iter().map(|p| p.0).collect();
                self.names.retain(|s, _| live.contains(s));
                self.remotes.retain(|s, _| live.contains(s));
            }
            Control::Start { seed, playlist, squad } => { if let Some(p) = playlist { self.playlist = p; } self.squad = squad; if !self.started { self.started = true; self.dead = false; self.dbno = false; self.reviver = None; self.health = MAX_HEALTH; self.shield = 0.; self.reported = (100, 0); self.events.push(Event::Started { seed }); } }
            Control::Left { slot } => {
                let name = self.name_of(slot);
                self.remotes.remove(&slot);
                self.names.remove(&slot);
                self.events.push(Event::Left { slot, name });
            }
            Control::End { reason } | Control::Kicked { reason } | Control::Error { code: reason } => { if self.ended.is_none() { self.ended = Some(reason.clone()); self.events.push(Event::Ended(reason)); } }
        }
    }

    pub fn tick_send(&mut self, dt: f32) -> bool {
        self.send_accum += dt;
        if self.send_accum >= 1. / SEND_HZ { self.send_accum -= 1. / SEND_HZ; if self.send_accum >= 1. / SEND_HZ { self.send_accum = 0.; } true } else { false }
    }
    pub fn send_state(&mut self, mut s: State, now_ms: f64) {
        self.seq = self.seq.wrapping_add(1);
        s.seq = self.seq;
        s.t_ms = now_ms as u32;
        self.outbox.push(proto::encode(&Msg::State(s)));
    }
    pub fn send_fire(&mut self, weapon: u8, origin: Vec3, dir: Vec3, flags: u8) -> u32 {
        self.fire_seq = self.fire_seq.wrapping_add(1);
        self.outbox.push(proto::encode(&Msg::Fire(Fire { seq: self.fire_seq, weapon, flags, origin, dir: dir.normalize_or_zero() })));
        self.fire_seq
    }
    pub fn send_hit(&mut self, h: Hit) { self.outbox.push(proto::encode(&Msg::Hit(h))); }
    pub fn send_build(&mut self, b: &Build) { self.outbox.push(proto::encode(&Msg::Build(b.clone()))); }
    pub fn send_pickup_spawn(&mut self, flags: u8, item: &str, count: u32, start: Vec3, end: Vec3) { self.outbox.push(proto::encode(&Msg::PickupSpawn { flags, count: count.clamp(1, 9999) as u16, start, end, item: item.to_owned() })); }
    pub fn send_pickup_claim(&mut self, id: u32) { self.outbox.push(proto::encode(&Msg::PickupClaim { id })); }
    pub fn send_revive(&mut self, target: u8, on: bool) { self.outbox.push(proto::encode(&Msg::Revive { target, on })); }
    pub fn team_of(&self, slot: u8) -> u8 { if slot == self.slot { self.team } else { self.teams.get(&slot).copied().unwrap_or(slot) } }
    pub fn is_teammate(&self, slot: u8) -> bool { slot != self.slot && slot != proto::NO_KILLER && self.team_of(slot) == self.team }
    pub fn alive(&self, slot: u8) -> bool { if slot == self.slot { !self.dead } else { self.remotes.get(&slot).map_or(self.names.contains_key(&slot), |r| !r.dead) } }
    pub fn knocked(&self, slot: u8) -> bool { if slot == self.slot { self.dbno } else { self.remotes.get(&slot).map_or(false, |r| r.dbno) } }
    pub fn teams_alive(&self) -> usize { let mut t: Vec<u8> = self.names.keys().copied().filter(|s| self.alive(*s)).map(|s| self.team_of(s)).collect(); t.sort(); t.dedup(); t.len() }
    pub fn teammates(&self) -> Vec<u8> { self.names.keys().copied().filter(|s| self.is_teammate(*s)).collect() }
    pub fn send_emote(&mut self, op: u8, item: u8) { self.outbox.push(proto::encode(&Msg::Emote { op, item })); }
    pub fn send_interact(&mut self, kind: u8, id: u32) { self.outbox.push(proto::encode(&Msg::Interact { kind, id })); }
    pub fn send_storm(&mut self, s: Storm) { if self.host { self.outbox.push(proto::encode(&Msg::Storm(s))); } }

    pub fn report_local_vitals(&mut self, health: f32, shield: f32) {
        if self.dead { return; }
        let (h, s) = (to_u8(health), to_u8(shield));
        let auth = (to_u8(self.health), to_u8(self.shield));
        if (h, s) == auth || (h, s) == self.reported { return; }
        self.reported = (h, s);
        let m = if h as u16 + s as u16 >= auth.0 as u16 + auth.1 as u16 { Msg::Heal { health: h, shield: s } } else { Msg::SelfDamage { health: h, shield: s } };
        self.outbox.push(proto::encode(&m));
    }

    pub fn recv(&mut self, bytes: &[u8], now_ms: f64) {
        let Some((from, m)) = proto::decode_relayed(bytes) else { return };
        self.handle(from, m, now_ms);
    }

    fn handle(&mut self, from: u8, m: Msg, now_ms: f64) {
        match m {
            Msg::State(s) => {
                if from == self.slot { return; }
                let r = self.remotes.entry(from).or_insert_with(|| Remote { health: MAX_HEALTH, ..Default::default() });
                r.name = self.names.get(&from).cloned().unwrap_or_default();
                r.seen = true;
                r.interp.push(s, now_ms);
            }
            Msg::Fire(f) => if from != self.slot { self.events.push(Event::RemoteFire { slot: from, fire: f }); },
            Msg::Build(b) => if from != self.slot { self.events.push(Event::Build { slot: from, build: b }); },
            Msg::Interact { kind, id } => if from != self.slot { self.events.push(Event::Interact { slot: from, kind, id }); },
            Msg::Emote { op, item } => if from != self.slot { self.events.push(Event::Emote { slot: from, op, item }); },
            Msg::Hit(_) | Msg::Heal { .. } | Msg::SelfDamage { .. } | Msg::Revive { .. } => {}
            Msg::Dbno { victim, state, by, weapon, head } => {
                if from != proto::SERVER_SLOT { return; }
                use proto::dbno_state as d;
                let set = |s: &mut Session, dbno: Option<bool>, reviver: Option<Option<u8>>| {
                    if victim == s.slot { if let Some(x) = dbno { s.dbno = x; } if let Some(r) = reviver { s.reviver = r; } }
                    else if let Some(rm) = s.remotes.get_mut(&victim) { if let Some(x) = dbno { rm.dbno = x; } if let Some(r) = reviver { rm.reviver = r; } }
                };
                match state {
                    d::DOWNED => { set(self, Some(true), Some(None)); self.events.push(Event::Knocked { victim, by, weapon, head }); }
                    d::REVIVED => { set(self, Some(false), Some(None)); self.events.push(Event::Revived { victim, by }); }
                    d::REVIVE_START => { set(self, None, Some(Some(by))); self.events.push(Event::ReviveStarted { victim, by }); }
                    d::REVIVE_STOP => { set(self, None, Some(None)); self.events.push(Event::ReviveStopped { victim, by }); }
                    _ => {}
                }
            }
            Msg::Health(list) => if from == proto::SERVER_SLOT { self.apply_health(&list) },
            Msg::Elim { victim, killer, weapon, head, flags } => {
                if from != proto::SERVER_SLOT { return; }
                if victim == self.slot { self.dead = true; self.dbno = false; self.reviver = None; self.health = 0.; self.shield = 0.; }
                if let Some(r) = self.remotes.get_mut(&victim) { r.dead = true; r.dbno = false; r.reviver = None; }
                self.events.push(Event::Elim { victim, killer, weapon, head, flags });
            }
            Msg::Damage { victim, attacker, flags, amount } => {
                if from != proto::SERVER_SLOT { return; }
                let dbno = flags & proto::damage_flag::DBNO != 0;
                if victim == self.slot { self.events.push(Event::Damaged { attacker, amount, shield_hit: flags & 1 != 0, head: flags & 2 != 0, dbno }); }
                else if attacker == self.slot { self.events.push(Event::DamageShown { victim, amount, shield_hit: flags & 1 != 0, head: flags & 2 != 0, dbno }); }
                if victim != self.slot { self.events.push(Event::Hurt { victim }); }
            }
            Msg::Spawn { slot, pos } => {
                if from != proto::SERVER_SLOT { return; }
                if slot == self.slot { self.dead = false; self.health = MAX_HEALTH; self.shield = 0.; self.reported = (100, 0); self.events.push(Event::Respawn(pos)); }
                else if let Some(r) = self.remotes.get_mut(&slot) { r.dead = false; r.health = MAX_HEALTH; r.shield = 0.; r.interp.reset(); }
            }
            Msg::Storm(s) => if from != self.slot { self.events.push(Event::Storm(s)); },
            Msg::Correct(p) => self.events.push(Event::Correct(p)),
            Msg::Pickup { id, owner, flags, count, start, end, item } => if from == proto::SERVER_SLOT { self.events.push(Event::PickupSpawned { id, owner, flags, count, start, end, item }); },
            Msg::PickupGone { id, taker } => if from == proto::SERVER_SLOT { self.events.push(Event::PickupGone { id, taker }); },
            Msg::Loot(_) | Msg::Pong { .. } | Msg::PickupSpawn { .. } | Msg::PickupClaim { .. } => {}
        }
    }

    fn apply_health(&mut self, list: &[HealthEntry]) {
        for e in list {
            if e.slot == self.slot {
                self.health = e.health as f32; self.shield = e.shield as f32; self.dead = e.dead;
                self.reported = (e.health, e.shield);
                self.events.push(Event::OwnHealth { health: self.health, shield: self.shield, dead: e.dead });
            } else if let Some(r) = self.remotes.get_mut(&e.slot) { r.health = e.health as f32; r.shield = e.shield as f32; r.dead = e.dead; }
        }
    }

    pub fn host_tick(&mut self, dt: f32, storm: Option<Storm>) {
        if !self.host { return; }
        self.storm_in -= dt;
        if self.storm_in <= 0. { self.storm_in = 2.; if let Some(s) = storm { self.send_storm(s); } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::relayed;

    fn server(to: &mut Session, m: Msg) { to.recv(&relayed(proto::SERVER_SLOT, &proto::encode(&m)), 0.); }

    #[test] fn control_messages_parse() {
        assert_eq!(parse_control(r#"{"t":"welcome","code":"ABC234","slot":3,"host":false,"token":"x"}"#), Some(Control::Welcome { slot: 3, host: false, code: "ABC234".into(), team: None, playlist: 0, matched: false }));
        assert_eq!(parse_control(r#"{"t":"welcome","code":"ABC234","slot":1,"host":false,"team":4,"playlist":1,"match":true}"#), Some(Control::Welcome { slot: 1, host: false, code: "ABC234".into(), team: Some(4), playlist: 1, matched: true }));
        assert_eq!(parse_control(r#"{"t":"lobby","players":[{"slot":0,"name":"Host","host":true}]}"#), Some(Control::Lobby(vec![(0, "Host".into(), true, None)])));
        assert_eq!(parse_control(r#"{"t":"start","seed":5,"playlist":1,"squad":true}"#), Some(Control::Start { seed: 5, playlist: Some(1), squad: true }));
        assert_eq!(parse_control(r#"{"t":"host","slot":2}"#), Some(Control::Host { slot: 2 }));
        assert_eq!(parse_control(r#"{"t":"end","reason":"host_left"}"#), Some(Control::End { reason: "host_left".into() }));
    }

    #[test] fn server_damage_health_and_elim_reach_the_victim_and_the_shooter() {
        let mut v = Session::new(1, false, "ABCDEF".into());
        let mut k = Session::new(0, true, "ABCDEF".into());
        for s in [&mut v, &mut k] { s.control(Control::Lobby(vec![(0, "K".into(), true, None), (1, "V".into(), false, None)])); s.control(Control::Start { seed: 1, playlist: None, squad: false }); s.take_events(); }
        for s in [&mut v, &mut k] {
            server(s, Msg::Damage { victim: 1, attacker: 0, flags: 1 | 2, amount: 20. });
            server(s, Msg::Damage { victim: 1, attacker: 0, flags: 2, amount: 60. });
            server(s, Msg::Health(vec![HealthEntry { slot: 1, health: 40, shield: 0, dead: false }]));
        }
        let ev = v.take_events();
        assert!(ev.contains(&Event::Damaged { attacker: 0, amount: 20., shield_hit: true, head: true, dbno: false }));
        assert!(ev.contains(&Event::OwnHealth { health: 40., shield: 0., dead: false }));
        let ek = k.take_events();
        assert!(ek.contains(&Event::DamageShown { victim: 1, amount: 60., shield_hit: false, head: true, dbno: false }));
        for s in [&mut v, &mut k] { server(s, Msg::Elim { victim: 1, killer: 0, weapon: 1, head: false, flags: 0 }); }
        assert!(v.dead && v.take_events().contains(&Event::Elim { victim: 1, killer: 0, weapon: 1, head: false, flags: 0 }));
        server(&mut v, Msg::Spawn { slot: 1, pos: Vec3::ZERO });
        assert!(!v.dead && v.take_events().contains(&Event::Respawn(Vec3::ZERO)));
    }

    #[test] fn vitals_from_a_player_are_ignored() {
        let mut c = Session::new(1, false, "ABCDEF".into());
        c.recv(&relayed(0, &proto::encode(&Msg::Health(vec![HealthEntry { slot: 1, health: 1, shield: 0, dead: false }]))), 0.);
        c.recv(&relayed(0, &proto::encode(&Msg::Elim { victim: 1, killer: 0, weapon: 1, head: false, flags: 0 })), 0.);
        c.recv(&relayed(0, &proto::encode(&Msg::Dbno { victim: 1, state: proto::dbno_state::DOWNED, by: 0, weapon: 1, head: false })), 0.);
        c.recv(&relayed(0, &proto::encode(&Msg::Damage { victim: 1, attacker: 0, flags: 0, amount: 50. })), 0.);
        assert_eq!((c.health, c.dead, c.dbno), (MAX_HEALTH, false, false));
        assert!(c.take_events().is_empty());
    }

    #[test] fn local_vitals_claims_follow_the_authority() {
        let mut c = Session::new(1, false, "ABCDEF".into());
        server(&mut c, Msg::Health(vec![HealthEntry { slot: 1, health: 70, shield: 0, dead: false }]));
        c.take_events();
        c.report_local_vitals(70., 0.);
        assert!(c.take_outbox().is_empty(), "matching the authority sends nothing");
        c.report_local_vitals(85., 0.);
        assert_eq!(c.take_outbox()[0][0], proto::HEAL);
        c.report_local_vitals(60., 0.);
        assert_eq!(c.take_outbox()[0][0], proto::SELFDMG);
    }

    #[test] fn only_the_host_sends_storm() {
        let mut h = Session::new(0, true, "X".into());
        let mut c = Session::new(1, false, "X".into());
        h.host_tick(0.1, Some(Storm::default()));
        c.host_tick(0.1, Some(Storm::default()));
        assert_eq!(h.take_outbox()[0][0], proto::STORM);
        assert!(c.take_outbox().is_empty());
    }

    #[test] fn states_feed_the_remote_interpolator() {
        let mut c = Session::new(1, false, "X".into());
        c.control(Control::Lobby(vec![(0, "Host".into(), true, None), (1, "Me".into(), false, None)]));
        for k in 0..4 { c.recv(&relayed(0, &proto::encode(&Msg::State(State { seq: k + 1, t_ms: k * 50, pos: Vec3::new(k as f32, 2., 0.), ..Default::default() }))), k as f64 * 50.); }
        let r = &c.remotes[&0];
        assert_eq!(r.name, "Host");
        assert!(r.sample(500.).is_some());
        c.recv(&relayed(1, &proto::encode(&Msg::State(State::default()))), 0.);
        assert!(!c.remotes.contains_key(&1), "own echoes are ignored");
    }

    #[test] fn emotes_from_others_become_events_and_own_echoes_do_not() {
        let mut c = Session::new(1, false, "X".into());
        c.control(Control::Lobby(vec![(0, "Host".into(), true, None), (1, "Me".into(), false, None)]));
        c.send_emote(proto::emote_op::PLAY, 3);
        assert_eq!(c.take_outbox(), vec![vec![proto::EMOTE, proto::emote_op::PLAY, 3]]);
        c.recv(&relayed(0, &proto::encode(&Msg::Emote { op: proto::emote_op::PLAY, item: 1 })), 0.);
        c.recv(&relayed(0, &proto::encode(&Msg::Emote { op: proto::emote_op::STOP, item: 1 })), 10.);
        c.recv(&relayed(1, &proto::encode(&Msg::Emote { op: proto::emote_op::PLAY, item: 1 })), 20.);
        let ev: Vec<_> = c.take_events().into_iter().filter(|e| matches!(e, Event::Emote { .. })).collect();
        assert_eq!(ev, vec![Event::Emote { slot: 0, op: proto::emote_op::PLAY, item: 1 }, Event::Emote { slot: 0, op: proto::emote_op::STOP, item: 1 }]);
    }

    #[test] fn teams_and_dbno_state_follow_the_relay() {
        let mut c = Session::new(0, false, "X".into());
        c.control(Control::Welcome { slot: 1, host: false, code: "X".into(), team: Some(0), playlist: 1, matched: true });
        c.control(Control::Lobby(vec![(0, "Mate".into(), true, Some(0)), (1, "Me".into(), false, Some(0)), (2, "Foe".into(), false, Some(1))]));
        c.control(Control::Start { seed: 1, playlist: Some(1), squad: true });
        for s in [0u8, 2] { c.recv(&relayed(s, &proto::encode(&Msg::State(State::default()))), 0.); }
        c.take_events();
        assert!(c.is_teammate(0) && !c.is_teammate(2) && !c.is_teammate(1) && c.squad && c.teams_alive() == 2);
        assert_eq!(c.teammates(), vec![0]);
        server(&mut c, Msg::Dbno { victim: 0, state: proto::dbno_state::DOWNED, by: 2, weapon: 1, head: true });
        assert!(c.knocked(0) && c.take_events().contains(&Event::Knocked { victim: 0, by: 2, weapon: 1, head: true }));
        c.send_revive(0, true);
        assert_eq!(c.take_outbox()[0], vec![proto::REVIVE, 0, 1]);
        server(&mut c, Msg::Dbno { victim: 0, state: proto::dbno_state::REVIVE_START, by: 1, weapon: 0, head: false });
        assert_eq!(c.remotes[&0].reviver, Some(1));
        server(&mut c, Msg::Dbno { victim: 0, state: proto::dbno_state::REVIVED, by: 1, weapon: 0, head: false });
        assert!(!c.knocked(0) && c.remotes[&0].reviver.is_none());
        server(&mut c, Msg::Dbno { victim: 1, state: proto::dbno_state::DOWNED, by: 2, weapon: 1, head: false });
        assert!(c.dbno);
        server(&mut c, Msg::Damage { victim: 1, attacker: proto::NO_KILLER, flags: proto::damage_flag::DBNO, amount: 2. });
        assert!(c.take_events().contains(&Event::Damaged { attacker: proto::NO_KILLER, amount: 2., shield_hit: false, head: false, dbno: true }));
        server(&mut c, Msg::Elim { victim: 1, killer: 2, weapon: 1, head: false, flags: proto::elim_flag::WAS_DBNO | proto::elim_flag::BLED_OUT });
        server(&mut c, Msg::Elim { victim: 0, killer: 2, weapon: 1, head: false, flags: proto::elim_flag::TEAM_WIPE });
        assert!(c.dead && !c.dbno && c.teams_alive() == 1);
    }

    #[test] fn send_rate_is_twenty_hertz() {
        let mut s = Session::new(0, false, "X".into());
        let n = (0..120).filter(|_| s.tick_send(1. / 60.)).count();
        assert!((39..=41).contains(&n), "{n}");
    }
}
