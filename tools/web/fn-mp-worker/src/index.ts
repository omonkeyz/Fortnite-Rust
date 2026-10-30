import { cleanName } from './names';
import { cleanChat } from './chat';
import { WEAPONS } from './caps';
import { T, SIZE, MAX_BINARY, MAX_TEXT, MAX_PLAYERS, HOST_ONLY, SERVER_ONLY, decodeState, decodeFire, decodeHit, decodeBuild, validHostMsg, relay, correct, pong, damageMsg, elimMsg, healthMsg, dbnoMsg, DBNO_STATE, ELIM_FLAG, decodePickupSpawn, pickupMsg, pickupGoneMsg, PICKUP_FLAG, PickupSpawn } from './proto';
import { DBNO, PLAYLISTS, playlist, formTeams, takeParties, Playlist, QueuedParty } from './modes';

export interface Env {
  LOBBY: DurableObjectNamespace;
  LIMITER: DurableObjectNamespace;
  MM: DurableObjectNamespace;
  ALLOWED_ORIGINS: string;
  MM_WINDOW_MS?: string;
  WARMUP_MS?: string;
}

const CODE_ALPHABET = 'ABCDEFGHJKMNPQRSTUVWXYZ23456789';
const CODE_RE = /^[ABCDEFGHJKMNPQRSTUVWXYZ23456789]{6}$/;
const LOBBY_TTL_MS = 3 * 3600 * 1000;
const IDLE_LOBBY_MS = 45 * 60 * 1000;
const HELLO_TIMEOUT_MS = 10000;
const RESUME_GRACE_MS = 30000;
const KICK_STRIKES = 40;
const MAX_HEALTH = 100;
const AERIAL_FLAG = 0x8000;
const AERIAL_H = 150.0;
const AERIAL_UP = 60.0;
const AERIAL_DOWN = 100.0;
const MAX_SHIELD = 100;
const MATCH_JOIN_MS = 30000;
const TICK_MS = 100;
const REWIND_MS = 250;
const NO_KILLER = 0xff;
const CHAT_RATE = 1;
const CHAT_BURST = 4;
const NAME_RATE = 0.2;
const NAME_BURST = 2;
const DEATH_DROP_MS = 10000;
const WARMUP_MS = 90000;
const DAMAGE_WARMUP = 8;
const DEATH_DROP_MAX = 48;
const MAX_PICKUPS = 1500;
const PICKUP_START_M = 4;
const PICKUP_TOSS_M = 10;
const PICKUP_CLAIM_M = 6;

function randomCode(): string {
  const b = new Uint8Array(6);
  crypto.getRandomValues(b);
  let s = '';
  for (const x of b) s += CODE_ALPHABET[x % CODE_ALPHABET.length];
  return s;
}
function randomToken(): string {
  const b = new Uint8Array(24);
  crypto.getRandomValues(b);
  return btoa(String.fromCharCode(...b)).replace(/[+/=]/g, (c) => (c === '+' ? '-' : c === '/' ? '_' : ''));
}
function originAllowed(env: Env, origin: string | null): boolean {
  if (!origin) return false;
  return env.ALLOWED_ORIGINS.split(',').map((s) => s.trim()).some((o) => o && o.toLowerCase() === origin.toLowerCase());
}
function plain(status: number, text: string): Response {
  return new Response(text, { status, headers: { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' } });
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    if (url.pathname === '/health') return plain(200, 'ok');
    const pk = url.pathname.match(/^\/peek\/([A-Za-z0-9]{6})$/);
    if (pk) {
      const origin = req.headers.get('Origin');
      if (!originAllowed(env, origin)) return plain(403, 'origin not allowed');
      const headers = { 'Content-Type': 'application/json', 'Cache-Control': 'no-store', 'Access-Control-Allow-Origin': origin as string, 'Vary': 'Origin' };
      const code = pk[1].toUpperCase();
      if (!CODE_RE.test(code)) return new Response(JSON.stringify({ open: false }), { headers });
      const stub = env.LOBBY.get(env.LOBBY.idFromName(code));
      const r = await stub.fetch(new Request('https://lobby/peek', { headers: { 'X-MP-Op': 'peek' } }));
      return new Response(await r.text(), { headers });
    }
    if (req.headers.get('Upgrade')?.toLowerCase() !== 'websocket') return plain(426, 'websocket only');
    if (!originAllowed(env, req.headers.get('Origin'))) return plain(403, 'origin not allowed');
    if (url.pathname === '/create') {
      const ip = req.headers.get('CF-Connecting-IP') || '';
      if (!/^(127\.|::1$|::ffff:127\.|$)/.test(ip)) {
        const lim = env.LIMITER.get(env.LIMITER.idFromName('global'));
        const ok = await lim.fetch('https://limiter/check', { method: 'POST', body: ip });
        if (ok.status !== 200) return plain(429, 'too many lobbies');
      }
      for (let i = 0; i < 8; i++) {
        const code = randomCode();
        const stub = env.LOBBY.get(env.LOBBY.idFromName(code));
        const h = new Headers(req.headers);
        h.set('X-MP-Op', 'create'); h.set('X-MP-Code', code);
        const r = await stub.fetch(new Request(req.url, { headers: h }));
        if (r.status === 409) continue;
        return r;
      }
      return plain(503, 'no free lobby code');
    }
    const mm = url.pathname.match(/^\/match\/([A-Za-z0-9]{6})$/);
    if (mm) {
      const code = mm[1].toUpperCase();
      if (!CODE_RE.test(code)) return plain(404, 'bad code');
      const stub = env.LOBBY.get(env.LOBBY.idFromName('match-' + code));
      const h = new Headers(req.headers);
      h.set('X-MP-Op', 'matchjoin'); h.set('X-MP-Code', code);
      return stub.fetch(new Request(req.url, { headers: h }));
    }
    const m = url.pathname.match(/^\/join\/([A-Za-z0-9]{6})$/);
    if (m) {
      const code = m[1].toUpperCase();
      if (!CODE_RE.test(code)) return plain(404, 'bad code');
      const stub = env.LOBBY.get(env.LOBBY.idFromName(code));
      const h = new Headers(req.headers);
      h.set('X-MP-Op', 'join'); h.set('X-MP-Code', code);
      return stub.fetch(new Request(req.url, { headers: h }));
    }
    return plain(404, 'not found');
  },
};

export class Limiter {
  private hits = new Map<string, number[]>();
  constructor(_s: DurableObjectState, _e: Env) {}
  async fetch(req: Request): Promise<Response> {
    const key = await req.text();
    const now = Date.now();
    const d = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(key)));
    const k = Array.from(d.slice(0, 8)).join('.');
    const list = (this.hits.get(k) || []).filter((t) => now - t < 60000);
    if (list.length >= 6) { this.hits.set(k, list); return new Response('limited', { status: 429 }); }
    list.push(now);
    this.hits.set(k, list);
    if (this.hits.size > 5000) for (const [kk, v] of this.hits) if (!v.some((t) => now - t < 60000)) this.hits.delete(kk);
    return new Response('ok');
  }
}

class Bucket {
  tokens: number;
  last = Date.now();
  constructor(public rate: number, public burst: number) { this.tokens = burst; }
  take(n = 1): boolean {
    const now = Date.now();
    this.tokens = Math.min(this.burst, this.tokens + ((now - this.last) / 1000) * this.rate);
    this.last = now;
    if (this.tokens < n) return false;
    this.tokens -= n;
    return true;
  }
}

interface Track { t: number; x: number; y: number; z: number }

interface Player {
  slot: number;
  name: string;
  ws: WebSocket | null;
  token: string;
  host: boolean;
  joinedAt: number;
  goneAt: number;
  msgs: Bucket;
  states: Bucket;
  builds: Bucket;
  heals: Bucket;
  interacts: Bucket;
  emotes: Bucket;
  fires: Map<number, Bucket>;
  strikes: number;
  strikeAt: number;
  track: Track[];
  last: Track | null;
  teleportOk: boolean;
  aerial: boolean;
  recentFires: Map<number, { t: number; weapon: number; hits: Set<number> }>;
  alive: boolean;
  health: number;
  shield: number;
  team: number;
  ticket: string;
  dbno: boolean;
  dbnoAt: number;
  knocker: number;
  knockWeapon: number;
  reviver: number;
  reviveEnd: number;
  nextBleed: number;
  ready: boolean;
  outfit: string;
  playlist: number;
  diedAt: number;
  deathDrops: number;
  drops: Bucket;
}

interface WorldPickup { id: number; owner: number; spawn: PickupSpawn }

const OUTFIT_RE = /^\/Game\/Athena\/Heroes\/[A-Za-z0-9_./]{1,100}$/;

export class Lobby {
  private code = '';
  private created = false;
  private createdAt = 0;
  private phase: 'lobby' | 'match' = 'lobby';
  private warmupUntil = 0;
  private players = new Map<number, Player>();
  private pending = new Set<WebSocket>();
  private seed = 0;
  private chatBuckets = new Map<number, { token: string; chat: Bucket; name: Bucket }>();
  private kind: 'party' | 'match' = 'party';
  private pl: Playlist = PLAYLISTS[0];
  private expected = new Map<string, { team: number; leader: boolean; used: boolean }>();
  private queued = false;
  private joinTimer: ReturnType<typeof setTimeout> | null = null;
  private ticker: ReturnType<typeof setInterval> | null = null;
  private pickups = new Map<number, WorldPickup>();
  private nextPickup = 1;
  private queuedPlaylist = -1;

  constructor(private state: DurableObjectState, private env: Env) {}

  async fetch(req: Request): Promise<Response> {
    const op = req.headers.get('X-MP-Op');
    const code = req.headers.get('X-MP-Code') || '';
    if (op === 'peek') {
      const live = [...this.players.values()].filter((p) => p.ws);
      const leader = live.find((p) => p.host);
      return new Response(JSON.stringify({ open: this.created && this.phase === 'lobby' && !!leader, leader: leader ? leader.name : '', size: live.length, max: MAX_PLAYERS }), { headers: { 'Content-Type': 'application/json' } });
    }
    if (op === 'create' && this.created) return new Response('taken', { status: 409 });
    if (op === 'assign') {
      const a: any = await req.json().catch(() => null);
      if (!a || typeof a.match !== 'string' || !Array.isArray(a.tickets)) return new Response('bad', { status: 400 });
      this.queued = false;
      this.queuedPlaylist = -1;
      const live = [...this.players.values()].filter((q) => q.ws).sort((x, y) => x.slot - y.slot);
      live.forEach((q, i) => { const ticket = a.tickets[i]; const team = Array.isArray(a.teams) && Number.isInteger(a.teams[i]) ? a.teams[i] : a.team; if (typeof ticket === 'string') { try { q.ws?.send(JSON.stringify({ t: 'assign', match: a.match, ticket, team, playlist: a.playlist })); } catch (_) {} } });
      this.broadcastLobby();
      return new Response('ok');
    }
    if (op === 'init') {
      const a: any = await req.json().catch(() => null);
      if (!a || !Array.isArray(a.slots)) return new Response('bad', { status: 400 });
      if (this.created) return new Response('taken', { status: 409 });
      this.reset();
      this.created = true;
      this.createdAt = Date.now();
      this.kind = 'match';
      this.code = code;
      this.pl = playlist(a.playlist);
      this.seed = (Math.random() * 0xffffffff) >>> 0;
      for (const t of a.slots) this.expected.set(String(t.ticket), { team: t.team | 0, leader: !!t.leader, used: false });
      this.joinTimer = setTimeout(() => this.startMatch(), MATCH_JOIN_MS);
      await this.state.storage.setAlarm(Date.now() + 60000);
      return new Response('ok');
    }
    if (op === 'matchjoin' && (!this.created || this.kind !== 'match')) {
      const pair0 = new WebSocketPair();
      const [c0, s0] = Object.values(pair0) as [WebSocket, WebSocket];
      s0.accept();
      this.fail(s0, 'no_lobby');
      return new Response(null, { status: 101, webSocket: c0 });
    }
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair) as [WebSocket, WebSocket];
    (server as any).binaryType = "arraybuffer";
    server.accept();
    if (op === 'create') {
      this.reset();
      this.created = true;
      this.createdAt = Date.now();
      this.code = code;
      this.seed = (Math.random() * 0xffffffff) >>> 0;
      await this.state.storage.setAlarm(Date.now() + 60000);
    }
    this.attach(server, op === 'create');
    return new Response(null, { status: 101, webSocket: client });
  }

  private reset() {
    if (this.joinTimer) clearTimeout(this.joinTimer);
    if (this.ticker) clearInterval(this.ticker);
    this.joinTimer = null; this.ticker = null;
    this.expected.clear();
    this.kind = 'party';
    this.queued = false;
    this.queuedPlaylist = -1;
    this.pickups.clear();
    this.nextPickup = 1;
    this.pl = PLAYLISTS[0];
    for (const p of this.players.values()) { try { p.ws?.close(4000, 'closed'); } catch (_) {} }
    for (const w of this.pending) { try { w.close(4000, 'closed'); } catch (_) {} }
    this.players.clear();
    this.pending.clear();
    this.created = false;
    this.phase = 'lobby';
    this.warmupUntil = 0;
    this.code = '';
  }

  private fail(ws: WebSocket, code: string, close = 4004) {
    try { ws.send(JSON.stringify({ t: 'error', code })); } catch (_) {}
    try { ws.close(close, code); } catch (_) {}
  }

  private attach(ws: WebSocket, creator: boolean) {
    this.pending.add(ws);
    let player: Player | null = null;
    const timer = setTimeout(() => { if (!player) { this.pending.delete(ws); this.fail(ws, 'hello_timeout', 4008); } }, HELLO_TIMEOUT_MS);
    ws.addEventListener('message', (ev: MessageEvent) => {
      const data = ev.data;
      if (!player) {
        if (typeof data !== 'string' || data.length > MAX_TEXT) { this.pending.delete(ws); this.fail(ws, 'bad_hello', 4002); return; }
        let msg: any;
        try { msg = JSON.parse(data); } catch (_) { this.pending.delete(ws); this.fail(ws, 'bad_hello', 4002); return; }
        clearTimeout(timer);
        this.pending.delete(ws);
        player = this.hello(ws, msg, creator);
        return;
      }
      this.onMessage(player, data);
    });
    const gone = () => { clearTimeout(timer); this.pending.delete(ws); if (player && player.ws === ws) this.left(player); };
    ws.addEventListener('close', gone);
    ws.addEventListener('error', gone);
  }

  private hello(ws: WebSocket, msg: any, creator: boolean): Player | null {
    if (!this.created) { this.fail(ws, 'no_lobby'); return null; }
    if (!msg || msg.t !== 'hello') { this.fail(ws, 'bad_hello', 4002); return null; }
    let entry: { team: number; leader: boolean; used: boolean } | undefined;
    if (this.kind === 'match' && typeof msg.resume !== 'string') {
      entry = typeof msg.ticket === 'string' ? this.expected.get(msg.ticket) : undefined;
      if (!entry || entry.used || this.phase === 'match') { this.fail(ws, 'bad_ticket'); return null; }
    }
    if (typeof msg.resume === 'string' && !creator) {
      for (const p of this.players.values()) {
        if (!p.ws && p.token === msg.resume && Date.now() - p.goneAt < RESUME_GRACE_MS) {
          p.ws = ws; p.goneAt = 0; p.teleportOk = true;
          ws.send(JSON.stringify({ t: 'welcome', code: this.code, slot: p.slot, host: p.host, token: p.token, phase: this.phase, seed: this.seed, team: p.team, playlist: this.pl.rotator, match: this.kind === 'match' }));
          this.broadcastLobby();
          if (this.phase === 'match') for (const w of this.pickups.values()) { try { ws.send(pickupMsg(w.id, w.owner, w.spawn)); } catch (_) {} }
          return p;
        }
      }
      this.fail(ws, 'resume_failed'); return null;
    }
    const name = cleanName(msg.name);
    if (!name) { this.fail(ws, 'bad_name', 4002); return null; }
    if (this.players.size >= MAX_PLAYERS) { this.fail(ws, 'full', 4009); return null; }
    let slot = 0;
    while (this.players.has(slot)) slot++;
    let finalName = name;
    const taken = new Set([...this.players.values()].map((p) => p.name.toLowerCase()));
    for (let i = 2; taken.has(finalName.toLowerCase()); i++) finalName = (name.slice(0, 13) + ' ' + i).slice(0, 16);
    const host = this.kind === 'match' ? (!!entry!.leader && ![...this.players.values()].some((q) => q.host)) : creator;
    const p: Player = {
      slot, name: finalName, ws, token: randomToken(), host, joinedAt: Date.now(), goneAt: 0,
      msgs: new Bucket(90, 180), states: new Bucket(36, 40), builds: new Bucket(12, 24), heals: new Bucket(2, 4), interacts: new Bucket(10, 20), emotes: new Bucket(4, 8),
      fires: new Map(), strikes: 0, strikeAt: Date.now(), track: [], last: null, teleportOk: true, aerial: false, recentFires: new Map(), alive: true, health: MAX_HEALTH, shield: 0, team: entry ? entry.team : slot, ticket: '', dbno: false, dbnoAt: 0, knocker: NO_KILLER, knockWeapon: 0, reviver: NO_KILLER, reviveEnd: 0, nextBleed: 0,
      ready: false, outfit: '', playlist: 0, diedAt: 0, deathDrops: 0, drops: new Bucket(4, 12),
    };
    if (entry) { entry.used = true; p.ticket = msg.ticket; }
    this.players.set(slot, p);
    ws.send(JSON.stringify({ t: 'welcome', code: this.code, slot, host: p.host, token: p.token, phase: this.phase, seed: this.seed, name: finalName, team: p.team, playlist: this.pl.rotator, match: this.kind === 'match' }));
    this.broadcastLobby();
    if (this.kind === 'match' && [...this.expected.values()].every((e) => e.used)) this.startMatch();
    return p;
  }

  private lobbyView() {
    return { t: 'lobby', code: this.code, phase: this.phase, match: this.kind === 'match', queued: this.queued, players: [...this.players.values()].filter((p) => p.ws).map((p) => ({ slot: p.slot, name: p.name, host: p.host, ready: p.ready, outfit: p.outfit, playlist: p.playlist, team: p.team })) };
  }
  private broadcastLobby() { this.sendText(JSON.stringify(this.lobbyView())); }
  private sendText(s: string, except?: Player) {
    for (const p of this.players.values()) if (p !== except && p.ws) { try { p.ws.send(s); } catch (_) {} }
  }
  private sendBin(b: Uint8Array, except?: Player) {
    for (const p of this.players.values()) if (p !== except && p.ws) { try { p.ws.send(b); } catch (_) {} }
  }

  private left(p: Player) {
    p.ws = null;
    if (p.host && this.kind === 'match') {
      p.host = false;
      const next = [...this.players.values()].filter((q) => q.ws).sort((x, y) => x.slot - y.slot)[0];
      if (next) { next.host = true; try { next.ws?.send(JSON.stringify({ t: 'host', slot: next.slot })); } catch (_) {} }
    } else if (p.host) { this.end('host_left'); return; }
    if (this.phase === 'match') { p.goneAt = Date.now(); this.state.storage.setAlarm(Date.now() + RESUME_GRACE_MS + 1000); }
    else this.players.delete(p.slot);
    this.sendText(JSON.stringify({ t: 'left', slot: p.slot }));
    this.broadcastLobby();
    this.checkEmpty();
  }

  private checkEmpty() {
    const live = [...this.players.values()].some((p) => p.ws);
    if (!live) this.reset();
  }

  private end(reason: string) {
    const s = JSON.stringify({ t: 'end', reason });
    for (const p of this.players.values()) if (p.ws) { try { p.ws.send(s); p.ws.close(4001, reason); } catch (_) {} }
    this.reset();
  }

  async alarm() {
    if (!this.created) return;
    const now = Date.now();
    if (now - this.createdAt > LOBBY_TTL_MS) { this.end('ttl'); return; }
    if (this.phase === 'lobby' && now - this.createdAt > IDLE_LOBBY_MS) { this.end('idle'); return; }
    for (const p of [...this.players.values()]) {
      if (!p.ws && p.goneAt && now - p.goneAt > RESUME_GRACE_MS) { this.players.delete(p.slot); this.broadcastLobby(); }
    }
    this.checkEmpty();
    if (this.created) await this.state.storage.setAlarm(now + 60000);
  }

  private strike(p: Player, n: number, why: string) {
    const now = Date.now();
    p.strikes = Math.max(0, p.strikes - (now - p.strikeAt) / 1000) + n;
    p.strikeAt = now;
    if (p.strikes >= KICK_STRIKES && p.ws) {
      const ws = p.ws;
      try { ws.send(JSON.stringify({ t: 'kicked', reason: why })); } catch (_) {}
      if (p.host) { this.end('host_kicked'); return; }
      this.players.delete(p.slot);
      try { ws.close(4003, 'kicked'); } catch (_) {}
      this.sendText(JSON.stringify({ t: 'left', slot: p.slot, kicked: true }));
      this.broadcastLobby();
      this.checkEmpty();
    }
  }

  private onMessage(p: Player, data: string | ArrayBuffer) {
    if (typeof data !== "string" && typeof (data as any).arrayBuffer === "function" && !(data instanceof ArrayBuffer)) { (data as any).arrayBuffer().then((b: ArrayBuffer) => this.onMessage(p, b)); return; }
    if (!p.msgs.take()) { this.strike(p, 1, 'rate'); return; }
    if (typeof data === 'string') {
      if (data.length > MAX_TEXT) { this.strike(p, 5, 'size'); return; }
      let m: any;
      try { m = JSON.parse(data); } catch (_) { this.strike(p, 3, 'schema'); return; }
      this.onControl(p, m);
      return;
    }
    const raw: any = data;
    const len = raw.byteLength ?? 0;
    if (len > MAX_BINARY || len < 1) { this.strike(p, 5, 'size'); return; }
    const bytes = ArrayBuffer.isView(raw) ? new Uint8Array(raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength)) : new Uint8Array(raw.slice(0));
    const v = new DataView(bytes.buffer);
    const type = bytes[0];
    if (SERVER_ONLY.has(type)) { this.strike(p, 10, 'permission'); return; }
    if (HOST_ONLY.has(type)) {
      if (!p.host) { this.strike(p, 10, 'permission'); return; }
      if (!validHostMsg(type, v)) { this.strike(p, 3, 'schema'); return; }
      this.sendBin(relay(type, p.slot, bytes), p);
      return;
    }
    switch (type) {
      case T.STATE: return this.onState(p, v, bytes);
      case T.FIRE: return this.onFire(p, v, bytes);
      case T.HIT: return this.onHit(p, v, bytes);
      case T.BUILD: {
        const b = decodeBuild(v);
        if (!b) { this.strike(p, 3, 'schema'); return; }
        if (!p.builds.take()) { this.strike(p, 2, 'build_rate'); return; }
        this.sendBin(relay(type, p.slot, bytes), p);
        return;
      }
      case T.HEAL: case T.SELFDMG: {
        if (v.byteLength !== SIZE.HEAL || bytes[1] > 100 || bytes[2] > 100) { this.strike(p, 3, 'schema'); return; }
        if (!p.heals.take()) { this.strike(p, 2, 'heal_rate'); return; }
        this.onVitals(p, bytes[1], bytes[2], type === T.HEAL);
        return;
      }
      case T.INTERACT: {
        if (v.byteLength !== SIZE.INTERACT) { this.strike(p, 3, 'schema'); return; }
        if (!p.interacts.take()) { this.strike(p, 1, 'interact_rate'); return; }
        this.sendBin(relay(type, p.slot, bytes), p);
        return;
      }
      case T.REVIVE: {
        if (v.byteLength !== SIZE.REVIVE) { this.strike(p, 3, 'schema'); return; }
        if (!p.interacts.take()) { this.strike(p, 1, 'interact_rate'); return; }
        this.onRevive(p, bytes[1], bytes[2] !== 0);
        return;
      }
      case T.EMOTE: {
        if (v.byteLength !== SIZE.EMOTE || bytes[1] > 1) { this.strike(p, 3, 'schema'); return; }
        if (!p.emotes.take()) { this.strike(p, 1, 'emote_rate'); return; }
        if (this.phase !== 'match' || !p.alive || (p.dbno && bytes[1] !== 0)) return;
        this.sendBin(relay(type, p.slot, bytes), p);
        return;
      }
      case T.PICKUP_SPAWN: return this.onPickupSpawn(p, v);
      case T.PICKUP_CLAIM: {
        if (v.byteLength !== SIZE.PICKUP_CLAIM) { this.strike(p, 3, 'schema'); return; }
        if (!p.interacts.take()) { this.strike(p, 1, 'interact_rate'); return; }
        this.onPickupClaim(p, v.getUint32(1, true));
        return;
      }
      case T.PING: {
        if (v.byteLength !== SIZE.PING) { this.strike(p, 3, 'schema'); return; }
        try { p.ws?.send(pong(v.getUint32(1, true), Date.now())); } catch (_) {}
        return;
      }
    }
    this.strike(p, 3, 'unknown_type');
  }


  private onControl(p: Player, m: any) {
    if (!m || typeof m.t !== 'string') { this.strike(p, 3, 'schema'); return; }
    switch (m.t) {
      case 'start':
        if (!p.host) { this.strike(p, 10, 'permission'); return; }
        if (this.phase === 'match') return;
        this.phase = 'match';
        this.beginMatch();
        return;
      case 'queue': this.onQueue(p, m); return;
      case 'unqueue': this.onUnqueue(p); return;
      case 'kick': {
        if (!p.host) { this.strike(p, 10, 'permission'); return; }
        const t = this.players.get(m.slot);
        if (!t || t.host) return;
        this.players.delete(t.slot);
        try { t.ws?.send(JSON.stringify({ t: 'kicked', reason: 'host' })); t.ws?.close(4003, 'kicked'); } catch (_) {}
        this.sendText(JSON.stringify({ t: 'left', slot: t.slot, kicked: true }));
        this.broadcastLobby();
        return;
      }
      case 'end':
        if (!p.host) { this.strike(p, 10, 'permission'); return; }
        this.end('host_ended');
        return;
      case 'leave':
        try { p.ws?.close(1000, 'leave'); } catch (_) {}
        p.goneAt = 0;
        if (!p.host) this.players.delete(p.slot);
        this.left(p);
        return;
      case 'lobby':
        try { p.ws?.send(JSON.stringify(this.lobbyView())); } catch (_) {}
        return;
      case 'chat': return this.onChat(p, m.text);
      case 'name': return this.onRename(p, m.name);
      case 'member': return this.onMember(p, m);
    }
    this.strike(p, 3, 'unknown_control');
  }

  private talk(p: Player) {
    let b = this.chatBuckets.get(p.slot);
    if (!b || b.token !== p.token) { b = { token: p.token, chat: new Bucket(CHAT_RATE, CHAT_BURST), name: new Bucket(NAME_RATE, NAME_BURST) }; this.chatBuckets.set(p.slot, b); }
    return b;
  }
  private reject(p: Player, code: string) { try { p.ws?.send(JSON.stringify({ t: 'chat_reject', code })); } catch (_) {} }

  private onChat(p: Player, raw: unknown) {
    if (!this.talk(p).chat.take()) { this.reject(p, 'rate'); return; }
    const r = cleanChat(raw);
    if (!r.ok) { this.reject(p, r.code); return; }
    this.sendText(JSON.stringify({ t: 'chat', slot: p.slot, name: p.name, text: r.text }));
  }

  private onMember(p: Player, m: any) {
    if (typeof m.ready !== 'boolean' || (m.outfit !== undefined && (typeof m.outfit !== 'string' || (m.outfit !== '' && !OUTFIT_RE.test(m.outfit))))) { this.strike(p, 3, 'schema'); return; }
    if (m.playlist !== undefined && !(Number.isInteger(m.playlist) && m.playlist >= 0 && m.playlist <= 2)) { this.strike(p, 3, 'schema'); return; }
    if (this.phase !== 'lobby') return;
    const outfit = typeof m.outfit === 'string' ? m.outfit : p.outfit;
    const playlist = typeof m.playlist === 'number' ? m.playlist : p.playlist;
    if (p.ready === m.ready && p.outfit === outfit && p.playlist === playlist) return;
    p.ready = m.ready;
    p.outfit = outfit;
    p.playlist = playlist;
    this.broadcastLobby();
  }

  private onRename(p: Player, raw: unknown) {
    if (!this.talk(p).name.take()) { this.reject(p, 'rate'); return; }
    const name = cleanName(raw);
    if (!name) { this.reject(p, 'bad_name'); return; }
    if (name === p.name) return;
    const taken = new Set([...this.players.values()].filter((q) => q !== p).map((q) => q.name.toLowerCase()));
    let finalName = name;
    for (let i = 2; taken.has(finalName.toLowerCase()); i++) finalName = (name.slice(0, 13) + ' ' + i).slice(0, 16);
    const old = p.name;
    p.name = finalName;
    this.sendText(JSON.stringify({ t: 'renamed', slot: p.slot, old, name: finalName }));
    this.broadcastLobby();
  }

  private onState(p: Player, v: DataView, bytes: Uint8Array) {
    const s = decodeState(v);
    if (!s) { this.strike(p, 3, 'schema'); return; }
    if (!p.states.take()) { this.strike(p, 1, 'state_rate'); return; }
    const now = Date.now();
    const cur: Track = { t: now, x: s.x, y: s.y, z: s.z };
    const aerial = (s.flags & AERIAL_FLAG) !== 0;
    const switched = aerial !== p.aerial;
    p.aerial = aerial;
    if (p.teleportOk || !p.last || switched) {
      p.teleportOk = false;
      p.track = [cur];
      p.last = cur;
    } else {
      const bad = this.speedViolation(p, cur, aerial);
      if (bad) {
        this.strike(p, 4, bad);
        try { p.ws?.send(correct(p.last.x, p.last.y, p.last.z)); } catch (_) {}
        return;
      }
      p.last = cur;
      p.track.push(cur);
      while (p.track.length > 2 && now - p.track[1].t > 2000) p.track.shift();
    }
    this.sendBin(relay(T.STATE, p.slot, bytes), p);
  }

  private speedViolation(p: Player, c: Track, aerial = false): string | null {
    const [H, UP, DOWN] = aerial ? [AERIAL_H, AERIAL_UP, AERIAL_DOWN] : [8.0, 12.0, 45.0];
    const check = (a: Track, slackH: number, slackV: number): boolean => {
      const dt = Math.max(0, (c.t - a.t) / 1000);
      const dh = Math.hypot(c.x - a.x, c.z - a.z);
      const dy = c.y - a.y;
      return dh > H * dt + slackH || dy > UP * dt + slackV || -dy > DOWN * dt + slackV;
    };
    if (p.last && check(p.last, 6, 8)) return 'teleport';
    const old = p.track.find((a) => c.t - a.t >= 1000) || p.track[0];
    if (old && check(old, 2.5, 4)) return 'speed';
    return null;
  }

  private onFire(p: Player, v: DataView, bytes: Uint8Array) {
    const f = decodeFire(v);
    if (!f) { this.strike(p, 3, 'schema'); return; }
    if (!p.alive || p.dbno || this.phase !== 'match') return;
    const w = WEAPONS[f.weapon];
    if (!w) { this.strike(p, 3, 'weapon'); return; }
    let b = p.fires.get(f.weapon);
    if (!b) { b = new Bucket(w.rate * 1.25 + 0.2, Math.max(3, Math.ceil(w.rate * 0.6))); p.fires.set(f.weapon, b); }
    if (!b.take()) { this.strike(p, 2, 'fire_rate'); return; }
    const now = Date.now();
    p.recentFires.set(f.seq, { t: now, weapon: f.weapon, hits: new Set() });
    if (p.recentFires.size > 64) for (const [k, r] of p.recentFires) if (now - r.t > 4000) p.recentFires.delete(k);
    this.sendBin(relay(T.FIRE, p.slot, bytes), p);
  }

  private onHit(p: Player, v: DataView, bytes: Uint8Array) {
    const h = decodeHit(v);
    if (!h) { this.strike(p, 3, 'schema'); return; }
    const w = WEAPONS[h.weapon];
    const fire = p.recentFires.get(h.fireSeq);
    const target = this.players.get(h.target);
    if (!p.alive || p.dbno || this.phase !== 'match') return;
    if (!w || !fire || fire.weapon !== h.weapon || Date.now() - fire.t > 4000) { this.strike(p, 3, 'hit_no_fire'); return; }
    if (!target || target === p || !target.alive || !target.last || !p.last) return;
    if (fire.hits.has(h.target)) { this.strike(p, 3, 'hit_dup'); return; }
    if (target.team === p.team && !w.row.startsWith('Launcher_')) return;
    if (h.pellets > w.pellets || h.damage > w.maxShot * (h.pellets / w.pellets) + 0.01) { this.strike(p, 8, 'damage_cap'); return; }
    if (this.rewoundDistance(p, target) > w.rangeM + 12) { this.strike(p, 6, 'range'); return; }
    fire.hits.add(h.target);
    if (this.inWarmup()) { this.sendBin(damageMsg(target.slot, p.slot, DAMAGE_WARMUP | ((h.flags & 1) !== 0 ? 2 : 0), 0)); return; }
    this.damage(target, p.slot, Math.min(h.damage, w.maxShot), (h.flags & 1) !== 0, h.weapon);
  }

  private warmupMs(): number { const v = Number(this.env.WARMUP_MS); return this.env.WARMUP_MS !== undefined && Number.isFinite(v) && v >= 0 ? v : WARMUP_MS; }
  private inWarmup(): boolean { return this.phase === 'match' && Date.now() < this.warmupUntil; }
  private rewoundDistance(p: Player, target: Player): number {
    const now = Date.now();
    const from = p.last!;
    let best = Math.hypot(target.last!.x - from.x, target.last!.y - from.y, target.last!.z - from.z);
    for (const a of target.track) if (now - a.t <= REWIND_MS) best = Math.min(best, Math.hypot(a.x - from.x, a.y - from.y, a.z - from.z));
    return best;
  }

  private broadcastHealth() {
    this.sendBin(healthMsg([...this.players.values()].map((q) => ({ slot: q.slot, health: q.health, shield: q.shield, dead: !q.alive }))));
  }

  private beginMatch() {
    this.phase = 'match';
    const warmupMs = this.warmupMs();
    this.warmupUntil = Date.now() + warmupMs;
    this.pickups.clear();
    for (const q of this.players.values()) { q.diedAt = 0; q.deathDrops = 0; q.alive = true; q.dbno = false; q.reviver = NO_KILLER; q.health = MAX_HEALTH; q.shield = 0; q.teleportOk = true; q.recentFires.clear(); }
    this.sendText(JSON.stringify({ t: 'start', seed: this.seed, at: Date.now(), warmupMs, playlist: this.pl.rotator, squad: this.pl.squadPlay }));
    this.broadcastLobby();
    this.broadcastHealth();
    if (!this.ticker) this.ticker = setInterval(() => this.tick(), TICK_MS);
  }

  private startMatch() {
    if (this.kind !== 'match' || this.phase === 'match') return;
    if (this.joinTimer) { clearTimeout(this.joinTimer); this.joinTimer = null; }
    if (![...this.players.values()].some((q) => q.ws)) { this.reset(); return; }
    if (![...this.players.values()].some((q) => q.host)) {
      const first = [...this.players.values()].filter((q) => q.ws).sort((x, y) => x.slot - y.slot)[0];
      first.host = true;
      try { first.ws?.send(JSON.stringify({ t: 'host', slot: first.slot })); } catch (_) {}
    }
    this.beginMatch();
  }

  private async onQueue(p: Player, m: any) {
    if (!p.host) { this.strike(p, 10, 'permission'); return; }
    if (this.kind !== 'party' || this.phase !== 'lobby' || this.queued) return;
    if (!(Number.isInteger(m.playlist) && m.playlist >= 0 && m.playlist < PLAYLISTS.length)) { this.strike(p, 3, 'schema'); return; }
    const pl = playlist(m.playlist);
    const live = [...this.players.values()].filter((q) => q.ws);
    if (live.length > MAX_PLAYERS) { try { p.ws?.send(JSON.stringify({ t: 'queue_error', code: 'party_too_big' })); } catch (_) {} return; }
    this.queued = true;
    this.queuedPlaylist = pl.rotator;
    this.broadcastLobby();
    const stub = this.env.MM.get(this.env.MM.idFromName('mm-' + pl.rotator));
    const body = { code: this.code, size: live.length, fill: m.fill === true, names: live.map((q) => q.name), playlist: pl.rotator };
    try {
      const r = await stub.fetch('https://mm/enqueue', { method: 'POST', body: JSON.stringify(body) });
      if (r.status !== 200) throw new Error('mm ' + r.status);
      this.sendText(JSON.stringify({ t: 'queued', playlist: pl.rotator }));
    } catch (_) {
      this.queued = false;
      this.queuedPlaylist = -1;
      try { p.ws?.send(JSON.stringify({ t: 'queue_error', code: 'matchmaking' })); } catch (_) {}
      this.broadcastLobby();
    }
  }

  private async onUnqueue(p: Player) {
    if (!p.host) { this.strike(p, 10, 'permission'); return; }
    if (this.kind !== 'party' || this.phase !== 'lobby' || !this.queued || this.queuedPlaylist < 0) return;
    const rotator = this.queuedPlaylist;
    const stub = this.env.MM.get(this.env.MM.idFromName('mm-' + rotator));
    let removed = false;
    try {
      const r = await stub.fetch('https://mm/dequeue', { method: 'POST', body: JSON.stringify({ code: this.code, playlist: rotator }) });
      removed = r.status === 200;
    } catch (_) { removed = false; }
    if (!removed || !this.queued) { try { p.ws?.send(JSON.stringify({ t: 'unqueue_error', code: 'forming' })); } catch (_) {} return; }
    this.queued = false;
    this.queuedPlaylist = -1;
    this.sendText(JSON.stringify({ t: 'unqueued', playlist: rotator }));
    this.broadcastLobby();
  }

  private onPickupSpawn(p: Player, v: DataView) {
    const s = decodePickupSpawn(v);
    if (!s) { this.strike(p, 3, 'schema'); return; }
    if (this.phase !== 'match') return;
    const death = (s.flags & PICKUP_FLAG.DEATH) !== 0;
    if (death) {
      if (p.alive || !p.diedAt || Date.now() - p.diedAt > DEATH_DROP_MS || p.deathDrops >= DEATH_DROP_MAX) { this.strike(p, 2, 'pickup_death'); return; }
    } else {
      if (!p.alive || p.dbno) return;
      if (!p.drops.take()) { this.strike(p, 2, 'pickup_rate'); return; }
    }
    if (p.last && Math.hypot(s.start[0] - p.last.x, s.start[1] - p.last.y, s.start[2] - p.last.z) > PICKUP_START_M) { this.strike(p, 3, 'pickup_far'); return; }
    if (Math.hypot(s.end[0] - s.start[0], s.end[1] - s.start[1], s.end[2] - s.start[2]) > PICKUP_TOSS_M) { this.strike(p, 3, 'pickup_far'); return; }
    if (this.pickups.size >= MAX_PICKUPS) return;
    if (death) p.deathDrops++;
    const w: WorldPickup = { id: this.nextPickup++, owner: p.slot, spawn: s };
    this.pickups.set(w.id, w);
    this.sendBin(pickupMsg(w.id, w.owner, s));
  }

  private onPickupClaim(p: Player, id: number) {
    if (this.phase !== 'match') return;
    const w = this.pickups.get(id);
    if (!w || !p.alive || p.dbno) return;
    if (p.last && Math.hypot(w.spawn.end[0] - p.last.x, w.spawn.end[1] - p.last.y, w.spawn.end[2] - p.last.z) > PICKUP_CLAIM_M) return;
    this.pickups.delete(id);
    this.sendBin(pickupGoneMsg(id, p.slot));
  }

  private standingMates(t: Player): Player[] {
    return [...this.players.values()].filter((q) => q !== t && q.team === t.team && q.alive && !q.dbno);
  }

  private damage(t: Player, attacker: number, amount: number, head: boolean, weapon: number) {
    if (!t.alive || !(amount > 0) || this.inWarmup()) return;
    const hf = head ? 2 : 0;
    if (t.dbno) {
      if (Date.now() - t.dbnoAt < DBNO.INVULNERABLE_MS) return;
      const fromHealth = Math.min(t.health, amount);
      t.health -= fromHealth;
      if (fromHealth > 0) this.sendBin(damageMsg(t.slot, attacker, hf | 4, fromHealth));
      if (t.health <= 0) this.eliminate(t, attacker, weapon, head, ELIM_FLAG.WAS_DBNO);
      else this.broadcastHealth();
      return;
    }
    const fromShield = Math.min(t.shield, amount);
    t.shield -= fromShield;
    const fromHealth = Math.min(t.health, amount - fromShield);
    t.health -= fromHealth;
    if (fromShield > 0) this.sendBin(damageMsg(t.slot, attacker, 1 | hf, fromShield));
    if (fromHealth > 0) this.sendBin(damageMsg(t.slot, attacker, hf, fromHealth));
    if (t.health <= 0) this.down(t, attacker, weapon, head);
    else this.broadcastHealth();
  }

  private down(t: Player, attacker: number, weapon: number, head: boolean) {
    if (this.pl.squadPlay && this.standingMates(t).length > 0) {
      const now = Date.now();
      t.dbno = true; t.dbnoAt = now; t.health = DBNO.INITIAL_HEALTH; t.shield = 0;
      t.knocker = attacker; t.knockWeapon = weapon; t.reviver = NO_KILLER; t.nextBleed = now + DBNO.BLEED_TICK_MS;
      this.stopRevivesBy(t);
      this.sendBin(dbnoMsg(t.slot, DBNO_STATE.DOWNED, attacker, weapon, head));
      this.broadcastHealth();
      return;
    }
    this.eliminate(t, attacker, weapon, head, 0);
  }

  private stopRevivesBy(r: Player) {
    for (const q of this.players.values()) if (q.reviver === r.slot) { q.reviver = NO_KILLER; q.nextBleed = Date.now() + DBNO.BLEED_TICK_MS; this.sendBin(dbnoMsg(q.slot, DBNO_STATE.REVIVE_STOP, r.slot, 0, false)); }
  }

  private eliminate(t: Player, killer: number, weapon: number, head: boolean, flags: number) {
    if (!t.alive) return;
    const wasDbno = t.dbno;
    if (t.reviver !== NO_KILLER) this.sendBin(dbnoMsg(t.slot, DBNO_STATE.REVIVE_STOP, t.reviver, 0, false));
    t.alive = false; t.dbno = false; t.reviver = NO_KILLER; t.health = 0; t.shield = 0; t.diedAt = Date.now(); t.deathDrops = 0;
    this.stopRevivesBy(t);
    this.sendBin(elimMsg(t.slot, killer, weapon, head, flags | (wasDbno ? ELIM_FLAG.WAS_DBNO : 0)));
    if (!wasDbno && this.standingMates(t).length === 0) {
      for (const q of [...this.players.values()]) {
        if (q !== t && q.team === t.team && q.alive && q.dbno) this.eliminate(q, q.knocker, q.knockWeapon, false, ELIM_FLAG.TEAM_WIPE);
      }
    }
    this.broadcastHealth();
  }

  private onRevive(r: Player, target: number, on: boolean) {
    const t = this.players.get(target);
    if (!t || this.phase !== 'match') return;
    if (!on) {
      if (t.reviver === r.slot) { t.reviver = NO_KILLER; t.nextBleed = Date.now() + DBNO.BLEED_TICK_MS; this.sendBin(dbnoMsg(t.slot, DBNO_STATE.REVIVE_STOP, r.slot, 0, false)); }
      return;
    }
    if (t === r || t.team !== r.team || !t.alive || !t.dbno || !r.alive || r.dbno || t.reviver !== NO_KILLER) return;
    if (r.last && t.last && Math.hypot(r.last.x - t.last.x, r.last.y - t.last.y, r.last.z - t.last.z) > DBNO.REVIVE_RANGE_M) return;
    t.reviver = r.slot;
    t.reviveEnd = Date.now() + DBNO.REVIVE_MS;
    this.sendBin(dbnoMsg(t.slot, DBNO_STATE.REVIVE_START, r.slot, 0, false));
  }

  private tick() {
    if (this.phase !== 'match') return;
    const now = Date.now();
    for (const t of [...this.players.values()]) {
      if (!t.alive || !t.dbno) continue;
      if (t.reviver !== NO_KILLER) {
        const r = this.players.get(t.reviver);
        if (!r || !r.alive || r.dbno || !r.ws) { const by = t.reviver; t.reviver = NO_KILLER; t.nextBleed = now + DBNO.BLEED_TICK_MS; this.sendBin(dbnoMsg(t.slot, DBNO_STATE.REVIVE_STOP, by, 0, false)); continue; }
        if (now >= t.reviveEnd) {
          const by = t.reviver;
          t.dbno = false; t.reviver = NO_KILLER; t.health = DBNO.REVIVE_HEALTH; t.shield = 0; t.knocker = NO_KILLER;
          this.sendBin(dbnoMsg(t.slot, DBNO_STATE.REVIVED, by, 0, false));
          this.broadcastHealth();
        }
        continue;
      }
      if (now >= t.nextBleed) {
        t.nextBleed += DBNO.BLEED_TICK_MS;
        const amount = Math.min(t.health, DBNO.BLEED_DAMAGE);
        t.health -= amount;
        this.sendBin(damageMsg(t.slot, NO_KILLER, 4, amount));
        if (t.health <= 0) this.eliminate(t, t.knocker, t.knockWeapon, false, ELIM_FLAG.BLED_OUT);
        else this.broadcastHealth();
      }
    }
  }

  private onVitals(p: Player, health: number, shield: number, heal: boolean) {
    if (!p.alive || this.phase !== 'match') return;
    if (heal) {
      if (p.dbno) return;
      p.health = Math.max(p.health, Math.min(MAX_HEALTH, health));
      p.shield = Math.max(p.shield, Math.min(MAX_SHIELD, shield));
    } else {
      p.health = Math.min(p.health, health);
      p.shield = Math.min(p.shield, shield);
      if (p.health <= 0) { if (p.dbno) this.eliminate(p, NO_KILLER, 0, false, 0); else this.down(p, NO_KILLER, 0, false); return; }
    }
    this.broadcastHealth();
  }
}

export class Matchmaker {
  private queues = new Map<number, (QueuedParty & { at: number })[]>();
  private timers = new Map<number, ReturnType<typeof setTimeout>>();
  constructor(private state: DurableObjectState, private env: Env) {}
  private windowMs(): number { const v = Number(this.env.MM_WINDOW_MS); return Number.isFinite(v) && v >= 0 ? v : 10000; }
  async fetch(req: Request): Promise<Response> {
    const url = new URL(req.url);
    if (url.pathname === '/dequeue' && req.method === 'POST') {
      const d: any = await req.json().catch(() => null);
      if (!d || typeof d.code !== 'string' || !Number.isInteger(d.playlist)) return new Response('bad', { status: 400 });
      const pl = playlist(d.playlist);
      const list = this.queues.get(pl.rotator) || [];
      const rest = list.filter((x) => x.code !== d.code);
      if (rest.length === list.length) return new Response('not queued', { status: 409 });
      this.queues.set(pl.rotator, rest);
      if (!rest.length) { const t = this.timers.get(pl.rotator); if (t) clearTimeout(t); this.timers.delete(pl.rotator); }
      return new Response('ok');
    }
    if (url.pathname !== '/enqueue' || req.method !== 'POST') return new Response('not found', { status: 404 });
    const q: any = await req.json().catch(() => null);
    if (!q || typeof q.code !== 'string' || !Number.isInteger(q.size) || q.size < 1 || !Number.isInteger(q.playlist)) return new Response('bad', { status: 400 });
    const pl = playlist(q.playlist);
    const list = (this.queues.get(pl.rotator) || []).filter((x) => x.code !== q.code);
    list.push({ code: q.code, size: q.size, fill: q.fill === true, names: Array.isArray(q.names) ? q.names.map(String) : [], at: Date.now() });
    this.queues.set(pl.rotator, list);
    const total = list.reduce((n, x) => n + x.size, 0);
    if (total >= Math.min(pl.maxPlayers, MAX_PLAYERS)) this.form(pl);
    else if (!this.timers.has(pl.rotator)) this.timers.set(pl.rotator, setTimeout(() => this.form(pl), this.windowMs()));
    return new Response('ok');
  }
  private async form(pl: Playlist) {
    const t = this.timers.get(pl.rotator);
    if (t) clearTimeout(t);
    this.timers.delete(pl.rotator);
    const { take: parties, rest } = takeParties(this.queues.get(pl.rotator) || [], Math.min(pl.maxPlayers, MAX_PLAYERS));
    this.queues.set(pl.rotator, rest);
    if (rest.length && !this.timers.has(pl.rotator)) this.timers.set(pl.rotator, setTimeout(() => this.form(pl), this.windowMs()));
    if (!parties.length) return;
    const teams = formTeams(pl, parties);
    for (let attempt = 0; attempt < 8; attempt++) {
      const code = randomCode();
      const slots: { ticket: string; team: number; leader: boolean }[] = [];
      const perParty = parties.map((p, i) => {
        const tickets: string[] = [];
        for (let k = 0; k < p.size; k++) { const ticket = randomToken(); tickets.push(ticket); slots.push({ ticket, team: teams[i].teams[k], leader: i === 0 && k === 0 }); }
        return { code: p.code, team: teams[i].team, teams: teams[i].teams, tickets };
      });
      const stub = this.env.LOBBY.get(this.env.LOBBY.idFromName('match-' + code));
      const r = await stub.fetch(new Request('https://lobby/init', { method: 'POST', headers: { 'X-MP-Op': 'init', 'X-MP-Code': code }, body: JSON.stringify({ playlist: pl.rotator, slots }) }));
      if (r.status === 409) continue;
      for (const pp of perParty) {
        const party = this.env.LOBBY.get(this.env.LOBBY.idFromName(pp.code));
        await party.fetch(new Request('https://lobby/assign', { method: 'POST', headers: { 'X-MP-Op': 'assign', 'X-MP-Code': pp.code }, body: JSON.stringify({ match: code, tickets: pp.tickets, team: pp.team, teams: pp.teams, playlist: pl.rotator }) })).catch(() => null);
      }
      return;
    }
  }
}
