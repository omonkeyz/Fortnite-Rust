export interface Playlist { rotator: number; id: number; name: string; teamSize: number; maxPlayers: number; squadPlay: boolean; fill: 'off' | 'on' | 'choice' }

export const PLAYLISTS: Playlist[] = [
  { rotator: 0, id: 2, name: 'Solo', teamSize: 1, maxPlayers: 100, squadPlay: false, fill: 'off' },
  { rotator: 1, id: 10, name: 'Duo', teamSize: 2, maxPlayers: 100, squadPlay: true, fill: 'on' },
  { rotator: 2, id: 9, name: 'Squad', teamSize: 4, maxPlayers: 100, squadPlay: true, fill: 'choice' },
];

export const DBNO = {
  INITIAL_HEALTH: 100,
  BLEED_DAMAGE: 2,
  BLEED_TICK_MS: 2000,
  REVIVE_HEALTH: 30,
  REVIVE_MS: 10000,
  INVULNERABLE_MS: 2000,
  REVIVE_RANGE_M: 4,
} as const;

export function playlist(rotator: number): Playlist { return PLAYLISTS[Math.max(0, Math.min(PLAYLISTS.length - 1, rotator | 0))]; }

export function wantsFill(p: Playlist, fill: boolean): boolean { return p.fill === 'on' || (p.fill === 'choice' && fill); }

export interface QueuedParty { code: string; size: number; fill: boolean; names: string[] }

export function formTeams(p: Playlist, parties: QueuedParty[]): { code: string; team: number; teams: number[] }[] {
  const teams: { size: number; fill: boolean }[] = [];
  const out: { code: string; team: number; teams: number[] }[] = [];
  for (const q of parties) {
    const fill = wantsFill(p, q.fill);
    const members: number[] = [];
    for (let k = 0; k < q.size; k += p.teamSize) {
      const n = Math.min(p.teamSize, q.size - k);
      let t = fill ? teams.findIndex((x) => x.fill && x.size + n <= p.teamSize) : -1;
      if (t < 0) { t = teams.length; teams.push({ size: 0, fill }); }
      teams[t].size += n;
      for (let j = 0; j < n; j++) members.push(t);
    }
    out.push({ code: q.code, team: members[0], teams: members });
  }
  return out;
}

export function takeParties<T extends { size: number }>(queue: T[], max: number): { take: T[]; rest: T[] } {
  const take: T[] = [];
  const rest: T[] = [];
  let n = 0;
  for (const q of queue) { if (!take.length || n + q.size <= max) { take.push(q); n += q.size; } else rest.push(q); }
  return { take, rest };
}
