export const T = {
  STATE: 0x01,
  FIRE: 0x02,
  HIT: 0x03,
  BUILD: 0x04,
  HEAL: 0x05,
  SELFDMG: 0x06,
  INTERACT: 0x07,
  REVIVE: 0x08,
  PING: 0x09,
  PICKUP_SPAWN: 0x0a,
  PICKUP_CLAIM: 0x0b,
  EMOTE: 0x0c,
  HEALTH: 0x40,
  ELIM: 0x41,
  STORM: 0x42,
  SPAWN: 0x43,
  LOOT: 0x44,
  DAMAGE: 0x45,
  DBNO: 0x46,
  PICKUP: 0x47,
  PICKUP_GONE: 0x48,
  CORRECT: 0x80,
  PONG: 0x89,
} as const;

export const SERVER_SLOT = 0xff;
export const MAX_BINARY = 256;
export const MAX_TEXT = 512;
export const MAX_PLAYERS = 16;

export const SIZE = {
  STATE: 45,
  FIRE: 31,
  HIT: 13,
  BUILD_HEAD: 36,
  HEAL: 3,
  SELFDMG: 3,
  INTERACT: 6,
  REVIVE: 3,
  PING: 5,
  ELIM: 5,
  STORM: 32,
  SPAWN: 14,
  DAMAGE: 8,
  PICKUP_SPAWN_HEAD: 29,
  PICKUP_CLAIM: 5,
  EMOTE: 3,
  PICKUP_HEAD: 35,
} as const;

export const HOST_ONLY = new Set<number>([T.STORM, T.LOOT]);
export const SERVER_ONLY = new Set<number>([T.HEALTH, T.ELIM, T.SPAWN, T.DAMAGE, T.DBNO, T.PICKUP, T.PICKUP_GONE, T.CORRECT, T.PONG]);

export function damageMsg(victim: number, attacker: number, flags: number, amount: number): Uint8Array {
  const out = new Uint8Array(9);
  const v = new DataView(out.buffer);
  out[0] = T.DAMAGE; out[1] = SERVER_SLOT; out[2] = victim; out[3] = attacker; out[4] = flags;
  v.setFloat32(5, amount, true);
  return out;
}

export const ELIM_FLAG = { WAS_DBNO: 1, BLED_OUT: 2, TEAM_WIPE: 4 } as const;

export function elimMsg(victim: number, killer: number, weapon: number, head: boolean, flags = 0): Uint8Array {
  return new Uint8Array([T.ELIM, SERVER_SLOT, victim, killer, weapon, head ? 1 : 0, flags]);
}

export const DBNO_STATE = { REVIVED: 0, DOWNED: 1, REVIVE_START: 2, REVIVE_STOP: 3 } as const;

export function dbnoMsg(victim: number, state: number, instigator: number, weapon: number, head: boolean): Uint8Array {
  return new Uint8Array([T.DBNO, SERVER_SLOT, victim, state, instigator, weapon, head ? 1 : 0]);
}

export function healthMsg(list: { slot: number; health: number; shield: number; dead: boolean }[]): Uint8Array {
  const n = Math.min(MAX_PLAYERS, list.length);
  const out = new Uint8Array(3 + 4 * n);
  out[0] = T.HEALTH; out[1] = SERVER_SLOT; out[2] = n;
  for (let i = 0; i < n; i++) {
    const e = list[i];
    out[3 + i * 4] = e.slot;
    out[4 + i * 4] = Math.max(0, Math.min(100, Math.ceil(e.health)));
    out[5 + i * 4] = Math.max(0, Math.min(100, Math.ceil(e.shield)));
    out[6 + i * 4] = e.dead ? 1 : 0;
  }
  return out;
}

export function spawnMsg(slot: number, x: number, y: number, z: number): Uint8Array {
  const out = new Uint8Array(15);
  const v = new DataView(out.buffer);
  out[0] = T.SPAWN; out[1] = SERVER_SLOT; out[2] = slot;
  v.setFloat32(3, x, true); v.setFloat32(7, y, true); v.setFloat32(11, z, true);
  return out;
}

export interface State { seq: number; tms: number; x: number; y: number; z: number; vx: number; vy: number; vz: number; yaw: number; pitch: number; flags: number; held: number; extra: number }
export interface Fire { seq: number; weapon: number; flags: number; ox: number; oy: number; oz: number; dx: number; dy: number; dz: number }
export interface Hit { fireSeq: number; target: number; weapon: number; flags: number; pellets: number; damage: number }
export interface Build { op: number; kind: number; material: number; rot: number; cell: [number, number, number]; loc: [number, number, number]; yaw: number; rotIter: number; mirrored: number; classPath: string }

const finite = (...v: number[]) => v.every((x) => Number.isFinite(x));

export function decodeState(b: DataView): State | null {
  if (b.byteLength !== SIZE.STATE) return null;
  const s: State = {
    seq: b.getUint32(1, true), tms: b.getUint32(5, true),
    x: b.getFloat32(9, true), y: b.getFloat32(13, true), z: b.getFloat32(17, true),
    vx: b.getFloat32(21, true), vy: b.getFloat32(25, true), vz: b.getFloat32(29, true),
    yaw: b.getFloat32(33, true), pitch: b.getFloat32(37, true),
    flags: b.getUint16(41, true), held: b.getUint8(43), extra: b.getUint8(44),
  };
  if (!finite(s.x, s.y, s.z, s.vx, s.vy, s.vz, s.yaw, s.pitch)) return null;
  if (Math.abs(s.x) > 6000 || Math.abs(s.z) > 6000 || s.y < -200 || s.y > 3000) return null;
  if (Math.hypot(s.vx, s.vz) > 60 || Math.abs(s.vy) > 60) return null;
  if (Math.abs(s.pitch) > 100 || Math.abs(s.yaw) > 1000) return null;
  return s;
}

export function decodeFire(b: DataView): Fire | null {
  if (b.byteLength !== SIZE.FIRE) return null;
  const f: Fire = {
    seq: b.getUint32(1, true), weapon: b.getUint8(5), flags: b.getUint8(6),
    ox: b.getFloat32(7, true), oy: b.getFloat32(11, true), oz: b.getFloat32(15, true),
    dx: b.getFloat32(19, true), dy: b.getFloat32(23, true), dz: b.getFloat32(27, true),
  };
  if (!finite(f.ox, f.oy, f.oz, f.dx, f.dy, f.dz)) return null;
  const l = Math.hypot(f.dx, f.dy, f.dz);
  if (l < 0.9 || l > 1.1) return null;
  return f;
}

export function decodeHit(b: DataView): Hit | null {
  if (b.byteLength !== SIZE.HIT) return null;
  const h: Hit = { fireSeq: b.getUint32(1, true), target: b.getUint8(5), weapon: b.getUint8(6), flags: b.getUint8(7), pellets: b.getUint8(8), damage: b.getFloat32(9, true) };
  if (!finite(h.damage) || h.damage <= 0 || h.pellets < 1) return null;
  return h;
}

const CLASS_RE = /^\/Game\/[A-Za-z0-9_\/.]{1,160}$/;

export function decodeBuild(b: DataView): Build | null {
  if (b.byteLength < SIZE.BUILD_HEAD) return null;
  const len = b.getUint8(35);
  if (b.byteLength !== SIZE.BUILD_HEAD + len) return null;
  const op = b.getUint8(1);
  if (op < 1 || op > 3) return null;
  const bytes = new Uint8Array(b.buffer, b.byteOffset + SIZE.BUILD_HEAD, len);
  let classPath = '';
  for (const c of bytes) classPath += String.fromCharCode(c);
  if (len > 0 && !CLASS_RE.test(classPath)) return null;
  const r: Build = {
    op, kind: b.getUint8(2), material: b.getUint8(3), rot: b.getUint8(4),
    cell: [b.getInt32(5, true), b.getInt32(9, true), b.getInt32(13, true)],
    loc: [b.getFloat32(17, true), b.getFloat32(21, true), b.getFloat32(25, true)],
    yaw: b.getFloat32(29, true), rotIter: b.getInt8(33), mirrored: b.getUint8(34), classPath,
  };
  if (!finite(...r.loc, r.yaw) || r.rot > 3 || r.material > 2 || r.kind > 7 || r.mirrored > 1) return null;
  if (r.cell.some((c) => Math.abs(c) > 100000) || r.loc.some((c) => Math.abs(c) > 600000)) return null;
  return r;
}

export function validHostMsg(type: number, b: DataView): boolean {
  switch (type) {
    case T.STORM: { if (b.byteLength !== SIZE.STORM) return false; for (let o = 4; o < 32; o += 4) if (!Number.isFinite(b.getFloat32(o, true))) return false; return true; }
    case T.LOOT: return b.byteLength >= 6 && b.byteLength <= 200;
  }
  return false;
}

export function relay(type: number, from: number, payload: Uint8Array): Uint8Array {
  const out = new Uint8Array(payload.byteLength + 1);
  out[0] = type;
  out[1] = from;
  out.set(payload.subarray(1), 2);
  return out;
}

export function correct(x: number, y: number, z: number): Uint8Array {
  const out = new Uint8Array(14);
  const v = new DataView(out.buffer);
  out[0] = T.CORRECT; out[1] = SERVER_SLOT;
  v.setFloat32(2, x, true); v.setFloat32(6, y, true); v.setFloat32(10, z, true);
  return out;
}

export function pong(nonce: number, nowMs: number): Uint8Array {
  const out = new Uint8Array(14);
  const v = new DataView(out.buffer);
  out[0] = T.PONG; out[1] = SERVER_SLOT;
  v.setUint32(2, nonce, true); v.setFloat64(6, nowMs, true);
  return out;
}

export const PICKUP_FLAG = { DEATH: 1, DROPPED: 2 } as const;
export const NO_TAKER = 0xff;
const ITEM_RE = /^[A-Za-z0-9_]{1,64}$/;

export interface PickupSpawn { flags: number; count: number; start: [number, number, number]; end: [number, number, number]; item: string }

export function decodePickupSpawn(b: DataView): PickupSpawn | null {
  if (b.byteLength < SIZE.PICKUP_SPAWN_HEAD) return null;
  const len = b.getUint8(28);
  if (b.byteLength !== SIZE.PICKUP_SPAWN_HEAD + len) return null;
  let item = '';
  for (let i = 0; i < len; i++) item += String.fromCharCode(b.getUint8(SIZE.PICKUP_SPAWN_HEAD + i));
  if (!ITEM_RE.test(item)) return null;
  const f = (o: number) => b.getFloat32(o, true);
  const r: PickupSpawn = { flags: b.getUint8(1), count: b.getUint16(2, true), start: [f(4), f(8), f(12)], end: [f(16), f(20), f(24)], item };
  if (!finite(...r.start, ...r.end) || r.count < 1 || r.count > 9999 || (r.flags & ~3) !== 0) return null;
  if ([r.start, r.end].some((v) => Math.abs(v[0]) > 6000 || Math.abs(v[2]) > 6000 || v[1] < -200 || v[1] > 3000)) return null;
  return r;
}

export function pickupMsg(id: number, owner: number, p: PickupSpawn): Uint8Array {
  const out = new Uint8Array(SIZE.PICKUP_HEAD + p.item.length);
  const v = new DataView(out.buffer);
  out[0] = T.PICKUP; out[1] = SERVER_SLOT;
  v.setUint32(2, id, true); out[6] = owner; out[7] = p.flags; v.setUint16(8, p.count, true);
  [...p.start, ...p.end].forEach((x, i) => v.setFloat32(10 + i * 4, x, true));
  out[34] = p.item.length;
  for (let i = 0; i < p.item.length; i++) out[SIZE.PICKUP_HEAD + i] = p.item.charCodeAt(i);
  return out;
}

export function pickupGoneMsg(id: number, taker: number): Uint8Array {
  const out = new Uint8Array(7);
  const v = new DataView(out.buffer);
  out[0] = T.PICKUP_GONE; out[1] = SERVER_SLOT; v.setUint32(2, id, true); out[6] = taker;
  return out;
}
