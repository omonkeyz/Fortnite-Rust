#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill { Off, On, Choice }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Playlist { pub rotator: usize, pub id: i32, pub name: &'static str, pub team_count: u32, pub team_size: u32, pub max_players: u32, pub squad_play: bool, pub fill: Fill }

pub const SOLO: Playlist = Playlist { rotator: 0, id: 2, name: "Solo", team_count: 100, team_size: 1, max_players: 100, squad_play: false, fill: Fill::Off };
pub const DUO: Playlist = Playlist { rotator: 1, id: 10, name: "Duo", team_count: 100, team_size: 2, max_players: 100, squad_play: true, fill: Fill::On };
pub const SQUAD: Playlist = Playlist { rotator: 2, id: 9, name: "Squad", team_count: 100, team_size: 4, max_players: 100, squad_play: true, fill: Fill::Choice };
pub const PLAYLISTS: [Playlist; 3] = [SOLO, DUO, SQUAD];

pub fn playlist(rotator: usize) -> Playlist { PLAYLISTS[rotator.min(PLAYLISTS.len() - 1)] }

pub fn max_team_info(id: i32) -> Option<(u32, u32, u32, u32)> {
    Some(match id {
        0 => (1, 4, 4, 4),
        2 => (100, 1, 1, 100),
        9 => (100, 4, 4, 100),
        10 => (100, 2, 2, 100),
        99 => (1, 1, 1, 1),
        -1 | 1 => (2, 5, 5, 10),
        _ => return None,
    })
}

pub fn squad_play(id: i32, max_team_size: u32, force: bool) -> bool { force || max_team_size > 1 || (9..=10).contains(&id) }

pub mod dbno {
    pub const INITIAL_HEALTH: f32 = 100.;
    pub const BLEED_DAMAGE: f32 = 2.;
    pub const BLEED_TICK_S: f32 = 2.;
    pub const REVIVE_HEALTH: f32 = 30.;
    pub const REVIVE_S: f32 = 10.;
    pub const INVULNERABLE_S: f32 = 2.;
    pub const REVIVE_RANGE_M: f32 = 4.;
    pub const SPECTATE_FIRST_S: f32 = 5.;
    pub const SPECTATE_RETRY_S: f32 = 2.;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn playlists_follow_get_max_team_info() {
        for p in PLAYLISTS {
            let (count, size, party, max) = max_team_info(p.id).unwrap();
            assert_eq!((count, size, max), (p.team_count, p.team_size, p.max_players), "{}", p.name);
            assert_eq!(party, size);
            assert_eq!(squad_play(p.id, p.team_size, false), p.squad_play, "{}", p.name);
        }
        assert!(max_team_info(3).is_none());
    }
    #[test] fn the_relay_uses_the_same_table() {
        let ts = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/web/fn-mp-worker/src/modes.ts")).unwrap();
        for p in PLAYLISTS {
            let fill = match p.fill { Fill::Off => "off", Fill::On => "on", Fill::Choice => "choice" };
            let line = format!("{{ rotator: {}, id: {}, name: '{}', teamSize: {}, maxPlayers: {}, squadPlay: {}, fill: '{}' }}", p.rotator, p.id, p.name, p.team_size, p.max_players, p.squad_play, fill);
            assert!(ts.contains(&line), "modes.ts lacks {line}");
        }
        for (k, v) in [("INITIAL_HEALTH", dbno::INITIAL_HEALTH), ("BLEED_DAMAGE", dbno::BLEED_DAMAGE), ("BLEED_TICK_MS", dbno::BLEED_TICK_S * 1000.), ("REVIVE_HEALTH", dbno::REVIVE_HEALTH), ("REVIVE_MS", dbno::REVIVE_S * 1000.), ("INVULNERABLE_MS", dbno::INVULNERABLE_S * 1000.), ("REVIVE_RANGE_M", dbno::REVIVE_RANGE_M)] {
            assert!(ts.contains(&format!("{k}: {v},")), "modes.ts {k} != {v}");
        }
    }
}
