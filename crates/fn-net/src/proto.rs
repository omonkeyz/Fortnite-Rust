use glam::Vec3;

pub const STATE: u8 = 0x01;
pub const FIRE: u8 = 0x02;
pub const HIT: u8 = 0x03;
pub const BUILD: u8 = 0x04;
pub const HEAL: u8 = 0x05;
pub const SELFDMG: u8 = 0x06;
pub const INTERACT: u8 = 0x07;
pub const REVIVE: u8 = 0x08;
pub const PING: u8 = 0x09;
pub const PICKUP_SPAWN: u8 = 0x0a;
pub const PICKUP_CLAIM: u8 = 0x0b;
pub const EMOTE: u8 = 0x0c;
pub const HEALTH: u8 = 0x40;
pub const ELIM: u8 = 0x41;
pub const STORM: u8 = 0x42;
pub const SPAWN: u8 = 0x43;
pub const LOOT: u8 = 0x44;
pub const DAMAGE: u8 = 0x45;
pub const DBNO: u8 = 0x46;
pub const PICKUP: u8 = 0x47;
pub const PICKUP_GONE: u8 = 0x48;
pub const CORRECT: u8 = 0x80;
pub const PONG: u8 = 0x89;
pub const SERVER_SLOT: u8 = 0xff;
pub const NO_KILLER: u8 = 0xff;

pub mod elim_flag { pub const WAS_DBNO: u8 = 1; pub const BLED_OUT: u8 = 2; pub const TEAM_WIPE: u8 = 4; }
pub mod dbno_state { pub const REVIVED: u8 = 0; pub const DOWNED: u8 = 1; pub const REVIVE_START: u8 = 2; pub const REVIVE_STOP: u8 = 3; }
pub mod pickup_flag { pub const DEATH: u8 = 1; pub const DROPPED: u8 = 2; }
pub const NO_TAKER: u8 = 0xff;
pub mod emote_op { pub const STOP: u8 = 0; pub const PLAY: u8 = 1; }
pub mod damage_flag { pub const SHIELD: u8 = 1; pub const HEAD: u8 = 2; pub const DBNO: u8 = 4; }

pub mod flag {
    pub const CROUCHED: u16 = 1;
    pub const RUNNING: u16 = 2;
    pub const AIRBORNE: u16 = 4;
    pub const TARGETING: u16 = 8;
    pub const BUILDING: u16 = 16;
    pub const RELOADING: u16 = 32;
    pub const SWINGING: u16 = 64;
    pub const HIDDEN: u16 = 128;
    pub const PICKAXE: u16 = 256;
    pub const AERIAL: u16 = 0x8000;
    pub const GUN_SHIFT: u16 = 9;
    pub const GUN_MASK: u16 = 0x3f;
    pub fn with_gun(flags: u16, gun: Option<usize>) -> u16 { (flags & !(GUN_MASK << GUN_SHIFT)) | (gun.map_or(0, |g| (g as u16 + 1).min(GUN_MASK)) << GUN_SHIFT) }
    pub fn gun(flags: u16) -> Option<usize> { let v = (flags >> GUN_SHIFT) & GUN_MASK; (v > 0).then(|| v as usize - 1) }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct State { pub seq: u32, pub t_ms: u32, pub pos: Vec3, pub vel: Vec3, pub yaw: f32, pub pitch: f32, pub flags: u16, pub held: u8, pub air_dm: u8 }

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fire { pub seq: u32, pub weapon: u8, pub flags: u8, pub origin: Vec3, pub dir: Vec3 }

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hit { pub fire_seq: u32, pub target: u8, pub weapon: u8, pub flags: u8, pub pellets: u8, pub damage: f32 }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Build { pub op: u8, pub kind: u8, pub material: u8, pub rot: u8, pub cell: [i32; 3], pub loc: Vec3, pub yaw: f32, pub rot_iter: i8, pub mirrored: bool, pub class_path: String }
pub mod build_op { pub const PLACE: u8 = 1; pub const EDIT: u8 = 2; pub const REMOVE: u8 = 3; }

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HealthEntry { pub slot: u8, pub health: u8, pub shield: u8, pub dead: bool }

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Storm { pub phase: u8, pub state: u8, pub center: [f32; 3], pub next: [f32; 3], pub time_left: f32 }

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    State(State),
    Fire(Fire),
    Hit(Hit),
    Build(Build),
    Heal { health: u8, shield: u8 },
    SelfDamage { health: u8, shield: u8 },
    Interact { kind: u8, id: u32 },
    Revive { target: u8, on: bool },
    Health(Vec<HealthEntry>),
    Elim { victim: u8, killer: u8, weapon: u8, head: bool, flags: u8 },
    Dbno { victim: u8, state: u8, by: u8, weapon: u8, head: bool },
    Storm(Storm),
    Spawn { slot: u8, pos: Vec3 },
    Loot(Vec<u8>),
    Damage { victim: u8, attacker: u8, flags: u8, amount: f32 },
    Correct(Vec3),
    Pong { nonce: u32, server_ms: f64 },
    PickupSpawn { flags: u8, count: u16, start: Vec3, end: Vec3, item: String },
    PickupClaim { id: u32 },
    Pickup { id: u32, owner: u8, flags: u8, count: u16, start: Vec3, end: Vec3, item: String },
    PickupGone { id: u32, taker: u8 },
    Emote { op: u8, item: u8 },
}

struct W(Vec<u8>);
impl W {
    fn new(t: u8) -> W { W(vec![t]) }
    fn u8(mut self, v: u8) -> W { self.0.push(v); self }
    fn i8(self, v: i8) -> W { self.u8(v as u8) }
    fn u16(mut self, v: u16) -> W { self.0.extend_from_slice(&v.to_le_bytes()); self }
    fn u32(mut self, v: u32) -> W { self.0.extend_from_slice(&v.to_le_bytes()); self }
    fn i32(mut self, v: i32) -> W { self.0.extend_from_slice(&v.to_le_bytes()); self }
    fn f32(mut self, v: f32) -> W { self.0.extend_from_slice(&v.to_le_bytes()); self }
    fn v3(self, v: Vec3) -> W { self.f32(v.x).f32(v.y).f32(v.z) }
}

struct R<'a> { b: &'a [u8], at: usize }
impl<'a> R<'a> {
    fn u8(&mut self) -> Option<u8> { let v = *self.b.get(self.at)?; self.at += 1; Some(v) }
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> { let s = self.b.get(self.at..self.at + N)?; self.at += N; s.try_into().ok() }
    fn u16(&mut self) -> Option<u16> { self.take::<2>().map(u16::from_le_bytes) }
    fn u32(&mut self) -> Option<u32> { self.take::<4>().map(u32::from_le_bytes) }
    fn i32(&mut self) -> Option<i32> { self.take::<4>().map(i32::from_le_bytes) }
    fn f32(&mut self) -> Option<f32> { self.take::<4>().map(f32::from_le_bytes).filter(|v| v.is_finite()) }
    fn f64(&mut self) -> Option<f64> { self.take::<8>().map(f64::from_le_bytes).filter(|v| v.is_finite()) }
    fn v3(&mut self) -> Option<Vec3> { Some(Vec3::new(self.f32()?, self.f32()?, self.f32()?)) }
    fn done(&self) -> bool { self.at == self.b.len() }
}

pub fn encode(m: &Msg) -> Vec<u8> {
    match m {
        Msg::State(s) => W::new(STATE).u32(s.seq).u32(s.t_ms).v3(s.pos).v3(s.vel).f32(s.yaw).f32(s.pitch).u16(s.flags).u8(s.held).u8(s.air_dm).0,
        Msg::Fire(f) => W::new(FIRE).u32(f.seq).u8(f.weapon).u8(f.flags).v3(f.origin).v3(f.dir).0,
        Msg::Hit(h) => W::new(HIT).u32(h.fire_seq).u8(h.target).u8(h.weapon).u8(h.flags).u8(h.pellets).f32(h.damage).0,
        Msg::Build(b) => {
            let cp: Vec<u8> = b.class_path.bytes().take(200).collect();
            let mut w = W::new(BUILD).u8(b.op).u8(b.kind).u8(b.material).u8(b.rot).i32(b.cell[0]).i32(b.cell[1]).i32(b.cell[2]).v3(b.loc).f32(b.yaw).i8(b.rot_iter).u8(b.mirrored as u8).u8(cp.len() as u8);
            w.0.extend_from_slice(&cp);
            w.0
        }
        Msg::Heal { health, shield } => W::new(HEAL).u8(*health).u8(*shield).0,
        Msg::SelfDamage { health, shield } => W::new(SELFDMG).u8(*health).u8(*shield).0,
        Msg::Interact { kind, id } => W::new(INTERACT).u8(*kind).u32(*id).0,
        Msg::Revive { target, on } => W::new(REVIVE).u8(*target).u8(*on as u8).0,
        Msg::Health(list) => { let mut w = W::new(HEALTH).u8(list.len().min(16) as u8); for e in list.iter().take(16) { w = w.u8(e.slot).u8(e.health.min(100)).u8(e.shield.min(100)).u8(e.dead as u8); } w.0 }
        Msg::Elim { victim, killer, weapon, head, flags } => W::new(ELIM).u8(*victim).u8(*killer).u8(*weapon).u8(*head as u8).u8(*flags).0,
        Msg::Dbno { victim, state, by, weapon, head } => W::new(DBNO).u8(*victim).u8(*state).u8(*by).u8(*weapon).u8(*head as u8).0,
        Msg::Storm(s) => W::new(STORM).u8(s.phase).u8(s.state).u8(0).f32(s.center[0]).f32(s.center[1]).f32(s.center[2]).f32(s.next[0]).f32(s.next[1]).f32(s.next[2]).f32(s.time_left).0,
        Msg::Spawn { slot, pos } => W::new(SPAWN).u8(*slot).v3(*pos).0,
        Msg::Loot(b) => { let mut w = W::new(LOOT); w.0.extend_from_slice(&b[..b.len().min(199)]); w.0 }
        Msg::Damage { victim, attacker, flags, amount } => W::new(DAMAGE).u8(*victim).u8(*attacker).u8(*flags).f32(*amount).0,
        Msg::Correct(p) => W::new(CORRECT).u8(SERVER_SLOT).v3(*p).0,
        Msg::PickupSpawn { flags, count, start, end, item } => { let id: Vec<u8> = item.bytes().take(64).collect(); let mut w = W::new(PICKUP_SPAWN).u8(*flags).u16(*count).v3(*start).v3(*end).u8(id.len() as u8); w.0.extend_from_slice(&id); w.0 }
        Msg::PickupClaim { id } => W::new(PICKUP_CLAIM).u32(*id).0,
        Msg::Pickup { id, owner, flags, count, start, end, item } => { let b: Vec<u8> = item.bytes().take(64).collect(); let mut w = W::new(PICKUP).u32(*id).u8(*owner).u8(*flags).u16(*count).v3(*start).v3(*end).u8(b.len() as u8); w.0.extend_from_slice(&b); w.0 }
        Msg::PickupGone { id, taker } => W::new(PICKUP_GONE).u32(*id).u8(*taker).0,
        Msg::Emote { op, item } => W::new(EMOTE).u8(*op).u8(*item).0,
        Msg::Pong { nonce, server_ms } => { let mut w = W::new(PONG).u8(SERVER_SLOT).u32(*nonce); w.0.extend_from_slice(&server_ms.to_le_bytes()); w.0 }
    }
}

pub fn ping(nonce: u32) -> Vec<u8> { W::new(PING).u32(nonce).0 }

pub fn relayed(slot: u8, client_bytes: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(client_bytes.len() + 1);
    v.push(client_bytes[0]);
    v.push(slot);
    v.extend_from_slice(&client_bytes[1..]);
    v
}

pub fn decode_relayed(b: &[u8]) -> Option<(u8, Msg)> {
    if b.len() < 2 { return None; }
    let (t, from) = (b[0], b[1]);
    let mut r = R { b, at: 2 };
    let m = match t {
        STATE => Msg::State(State { seq: r.u32()?, t_ms: r.u32()?, pos: r.v3()?, vel: r.v3()?, yaw: r.f32()?, pitch: r.f32()?, flags: r.u16()?, held: r.u8()?, air_dm: r.u8()? }),
        FIRE => Msg::Fire(Fire { seq: r.u32()?, weapon: r.u8()?, flags: r.u8()?, origin: r.v3()?, dir: r.v3()? }),
        HIT => Msg::Hit(Hit { fire_seq: r.u32()?, target: r.u8()?, weapon: r.u8()?, flags: r.u8()?, pellets: r.u8()?, damage: r.f32()? }),
        BUILD => {
            let (op, kind, material, rot) = (r.u8()?, r.u8()?, r.u8()?, r.u8()?);
            let cell = [r.i32()?, r.i32()?, r.i32()?];
            let (loc, yaw, rot_iter, mirrored, len) = (r.v3()?, r.f32()?, r.u8()? as i8, r.u8()? != 0, r.u8()? as usize);
            let s = b.get(r.at..r.at + len)?; r.at += len;
            Msg::Build(Build { op, kind, material, rot, cell, loc, yaw, rot_iter, mirrored, class_path: String::from_utf8(s.to_vec()).ok()? })
        }
        HEAL => Msg::Heal { health: r.u8()?, shield: r.u8()? },
        SELFDMG => Msg::SelfDamage { health: r.u8()?, shield: r.u8()? },
        INTERACT => Msg::Interact { kind: r.u8()?, id: r.u32()? },
        REVIVE => Msg::Revive { target: r.u8()?, on: r.u8()? != 0 },
        HEALTH => { let n = r.u8()? as usize; let mut v = vec![]; for _ in 0..n.min(16) { v.push(HealthEntry { slot: r.u8()?, health: r.u8()?, shield: r.u8()?, dead: r.u8()? & 1 != 0 }); } Msg::Health(v) }
        ELIM => Msg::Elim { victim: r.u8()?, killer: r.u8()?, weapon: r.u8()?, head: r.u8()? & 1 != 0, flags: r.u8()? },
        DBNO => Msg::Dbno { victim: r.u8()?, state: r.u8()?, by: r.u8()?, weapon: r.u8()?, head: r.u8()? & 1 != 0 },
        STORM => { let (phase, state, _) = (r.u8()?, r.u8()?, r.u8()?); Msg::Storm(Storm { phase, state, center: [r.f32()?, r.f32()?, r.f32()?], next: [r.f32()?, r.f32()?, r.f32()?], time_left: r.f32()? }) }
        SPAWN => Msg::Spawn { slot: r.u8()?, pos: r.v3()? },
        LOOT => { let v = b[2..].to_vec(); r.at = b.len(); Msg::Loot(v) }
        DAMAGE => Msg::Damage { victim: r.u8()?, attacker: r.u8()?, flags: r.u8()?, amount: r.f32()? },
        CORRECT => Msg::Correct(r.v3()?),
        PONG => Msg::Pong { nonce: r.u32()?, server_ms: r.f64()? },
        PICKUP => {
            let (id, owner, flags, count, start, end, len) = (r.u32()?, r.u8()?, r.u8()?, r.u16()?, r.v3()?, r.v3()?, r.u8()? as usize);
            let s = b.get(r.at..r.at + len)?; r.at += len;
            Msg::Pickup { id, owner, flags, count, start, end, item: String::from_utf8(s.to_vec()).ok()? }
        }
        PICKUP_GONE => Msg::PickupGone { id: r.u32()?, taker: r.u8()? },
        EMOTE => Msg::Emote { op: r.u8()?, item: r.u8()? },
        _ => return None,
    };
    if !r.done() { return None; }
    Some((from, m))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn roundtrip(m: Msg) { let b = encode(&m); let (s, d) = decode_relayed(&relayed(7, &b)).expect("decodes"); assert_eq!((s, d), (7, m)); }
    #[test] fn sizes_match_the_relay() {
        assert_eq!(encode(&Msg::State(State::default())).len(), 45);
        assert_eq!(encode(&Msg::Fire(Fire::default())).len(), 31);
        assert_eq!(encode(&Msg::Hit(Hit::default())).len(), 13);
        assert_eq!(encode(&Msg::Build(Build::default())).len(), 36);
        assert_eq!(encode(&Msg::Heal { health: 1, shield: 2 }).len(), 3);
        assert_eq!(encode(&Msg::Interact { kind: 1, id: 2 }).len(), 6);
        assert_eq!(ping(1).len(), 5);
        assert_eq!(encode(&Msg::Elim { victim: 1, killer: 2, weapon: 3, head: true, flags: 1 }).len(), 6);
        assert_eq!(encode(&Msg::Revive { target: 1, on: true }).len(), 3);
        assert_eq!(encode(&Msg::Dbno { victim: 1, state: 1, by: 2, weapon: 3, head: false }).len(), 6);
        assert_eq!(encode(&Msg::Storm(Storm::default())).len(), 32);
        assert_eq!(encode(&Msg::Spawn { slot: 1, pos: Vec3::ONE }).len(), 14);
        assert_eq!(encode(&Msg::Damage { victim: 1, attacker: 2, flags: 0, amount: 3. }).len(), 8);
        assert_eq!(encode(&Msg::PickupSpawn { flags: 1, count: 1, start: Vec3::ZERO, end: Vec3::ZERO, item: "Athena_Bandage".into() }).len(), 29 + 14);
        assert_eq!(encode(&Msg::PickupClaim { id: 3 }).len(), 5);
        assert_eq!(encode(&Msg::Emote { op: emote_op::PLAY, item: 0 }).len(), 3);
        assert_eq!(relayed(0xff, &encode(&Msg::Pickup { id: 1, owner: 0, flags: 0, count: 1, start: Vec3::ZERO, end: Vec3::ZERO, item: "Athena_Bandage".into() })).len(), 35 + 14);
    }
    #[test] fn every_message_roundtrips() {
        roundtrip(Msg::State(State { seq: 9, t_ms: 1000, pos: Vec3::new(1., 2., 3.), vel: Vec3::new(4., 5., 6.), yaw: 90., pitch: -10., flags: 5, held: 3, air_dm: 7 }));
        roundtrip(Msg::Fire(Fire { seq: 3, weapon: 2, flags: 1, origin: Vec3::ONE, dir: Vec3::X }));
        roundtrip(Msg::Hit(Hit { fire_seq: 3, target: 1, weapon: 2, flags: 1, pellets: 10, damage: 99.5 }));
        roundtrip(Msg::Build(Build { op: 1, kind: 2, material: 1, rot: 3, cell: [1, -2, 3], loc: Vec3::new(100., 200., 300.), yaw: 90., rot_iter: -1, mirrored: true, class_path: "/Game/Building/X.X_C".into() }));
        roundtrip(Msg::Health(vec![HealthEntry { slot: 1, health: 50, shield: 25, dead: false }, HealthEntry { slot: 2, health: 0, shield: 0, dead: true }]));
        roundtrip(Msg::Storm(Storm { phase: 2, state: 1, center: [1., 2., 3.], next: [4., 5., 6.], time_left: 30. }));
        roundtrip(Msg::Damage { victim: 1, attacker: 0, flags: 3, amount: 42. });
        roundtrip(Msg::Spawn { slot: 4, pos: Vec3::new(5., 6., 7.) });
        roundtrip(Msg::Elim { victim: 1, killer: 2, weapon: 3, head: true, flags: elim_flag::WAS_DBNO | elim_flag::TEAM_WIPE });
        roundtrip(Msg::Dbno { victim: 1, state: dbno_state::DOWNED, by: 2, weapon: 7, head: true });
        roundtrip(Msg::Revive { target: 3, on: true });
        roundtrip(Msg::Pickup { id: 9, owner: 2, flags: pickup_flag::DEATH, count: 30, start: Vec3::new(1., 2., 3.), end: Vec3::new(4., 0.8, 6.), item: "AthenaAmmoDataShells".into() });
        roundtrip(Msg::PickupGone { id: 9, taker: 1 });
        roundtrip(Msg::Emote { op: emote_op::PLAY, item: 2 });
        roundtrip(Msg::Emote { op: emote_op::STOP, item: 0 });
    }
    #[test] fn truncated_and_trailing_bytes_are_rejected() {
        let b = relayed(1, &encode(&Msg::Hit(Hit::default())));
        assert!(decode_relayed(&b[..b.len() - 1]).is_none());
        let mut long = b.clone(); long.push(0);
        assert!(decode_relayed(&long).is_none());
    }
    #[test] fn fixture_for_the_worker_test() {
        let b = encode(&Msg::State(State { seq: 1, t_ms: 2, pos: Vec3::new(10., 2., 10.), vel: Vec3::ZERO, yaw: 90., pitch: 0., flags: 0, held: 1, air_dm: 0 }));
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        assert_eq!(hex, "0101000000020000000000204100000040000020410000000000000000000000000000b4420000000000000100");
    }
}
