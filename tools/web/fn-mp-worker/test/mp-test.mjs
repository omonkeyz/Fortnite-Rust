import WebSocket from 'ws';

const BASE = process.env.MP_URL || 'ws://127.0.0.1:8798';
const ORIGIN = process.env.MP_ORIGIN || 'http://127.0.0.1:8797';
let failures = 0;
const ok = (cond, what) => { console.log((cond ? 'PASS ' : 'FAIL ') + what); if (!cond) failures++; };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class Client {
  constructor(path, origin = ORIGIN) {
    this.texts = []; this.bins = []; this.closed = null; this.waiters = [];
    this.ws = new WebSocket(BASE + path, { origin });
    this.ws.binaryType = 'arraybuffer';
    this.open = new Promise((res) => { this.ws.on('open', () => res(true)); this.ws.on('error', () => res(false)); this.ws.on('unexpected-response', (_q, r) => { this.status = r.statusCode; res(false); }); });
    this.ws.on('message', (d, isBin) => { if (isBin) this.bins.push(new Uint8Array(d)); else this.texts.push(JSON.parse(d.toString())); this.waiters.forEach((w) => w()); });
    this.ws.on('close', (code) => { this.closed = code; this.waiters.forEach((w) => w()); });
  }
  async until(pred, ms = 3000) {
    const t0 = Date.now();
    while (Date.now() - t0 < ms) { const v = pred(this); if (v) return v; await new Promise((r) => { this.waiters.push(r); setTimeout(r, 50); }); this.waiters = []; }
    return pred(this);
  }
  text(t, ms) { return this.until((c) => c.texts.find((m) => m.t === t), ms); }
  send(o) { this.ws.send(typeof o === 'string' || o instanceof Uint8Array ? o : JSON.stringify(o)); }
  close() { try { this.ws.close(); } catch (_) {} }
}

function state(seq, tms, x, y, z, vx = 0, vy = 0, vz = 0, flags = 0) {
  const b = new Uint8Array(45); const v = new DataView(b.buffer);
  b[0] = 0x01; v.setUint32(1, seq, true); v.setUint32(5, tms, true);
  [x, y, z, vx, vy, vz, 90, 0].forEach((f, i) => v.setFloat32(9 + i * 4, f, true));
  v.setUint16(41, flags, true); b[43] = 1; b[44] = 0;
  return b;
}
function fire(seq, weapon) {
  const b = new Uint8Array(31); const v = new DataView(b.buffer);
  b[0] = 0x02; v.setUint32(1, seq, true); b[5] = weapon; b[6] = 0;
  [0, 1.7, 0, 1, 0, 0].forEach((f, i) => v.setFloat32(7 + i * 4, f, true));
  return b;
}
function hit(fireSeq, target, weapon, damage, pellets = 1) {
  const b = new Uint8Array(13); const v = new DataView(b.buffer);
  b[0] = 0x03; v.setUint32(1, fireSeq, true); b[5] = target; b[6] = weapon; b[7] = 0; b[8] = pellets; v.setFloat32(9, damage, true);
  return b;
}
function health(slot, hp, sh) { return new Uint8Array([0x40, 1, slot, hp, sh, 0]); }
function heal(hp, sh) { return new Uint8Array([0x05, hp, sh]); }
function hitHead(fireSeq, target, weapon, damage) { const b = hit(fireSeq, target, weapon, damage); b[7] = 1; return b; }
const dmgs = (c) => c.bins.filter((b) => b[0] === 0x45).map((b) => ({ from: b[1], victim: b[2], attacker: b[3], flags: b[4], amount: new DataView(b.buffer, b.byteOffset).getFloat32(5, true) }));
const lastHealth = (c, slot) => { const hs = c.bins.filter((b) => b[0] === 0x40 && b[1] === 0xff); for (let k = hs.length - 1; k >= 0; k--) { const b = hs[k]; for (let i = 0; i < b[2]; i++) if (b[3 + i * 4] === slot) return { hp: b[4 + i * 4], sh: b[5 + i * 4], dead: b[6 + i * 4] === 1 }; } return null; };

async function create(name = 'HostPlayer') {
  const h = new Client('/create');
  await h.open;
  h.send({ t: 'hello', name });
  const w = await h.text('welcome');
  return { h, w };
}
async function join(code, name) {
  const c = new Client('/join/' + code);
  await c.open;
  c.send({ t: 'hello', name });
  await c.until((x) => x.texts.find((m) => m.t === 'welcome' || m.t === 'error') || x.closed);
  return c;
}

async function main() {
  ok(Buffer.from(state(1, 2, 10, 2, 10)).toString('hex') === '0101000000020000000000204100000040000020410000000000000000000000000000b4420000000000000100', 'STATE layout matches the fn-net fixture');
  {
    const bad = new Client('/create', 'https://evil.example');
    const opened = await bad.open;
    ok(!opened && bad.status === 403, 'bad origin rejected with 403');
  }
  const { h, w } = await create();
  ok(w && /^[A-Z2-9]{6}$/.test(w.code) && w.host === true && w.slot === 0, 'create returns 6-char code, host slot 0');
  ok(!/[ILO01]/.test(w.code), 'code alphabet has no ambiguous characters');
  {
    const c = await join('ZZZZZ2', 'Nobody');
    ok(c.texts.some((m) => m.t === 'error' && m.code === 'no_lobby'), 'bad code -> no_lobby');
  }
  {
    const c = await join(w.code, 'f u c k');
    ok(c.texts.some((m) => m.t === 'error' && m.code === 'bad_name'), 'profane name rejected');
    const c2 = await join(w.code, 'ab');
    ok(c2.texts.some((m) => m.t === 'error' && m.code === 'bad_name'), 'short name rejected');
    const c3 = await join(w.code, 'bad<name>');
    ok(c3.texts.some((m) => m.t === 'error' && m.code === 'bad_name'), 'charset whitelist');
  }
  const a = await join(w.code, 'Alpha');
  const aw = a.texts.find((m) => m.t === 'welcome');
  ok(aw && aw.slot === 1 && aw.host === false, 'join gets slot 1');
  const lob = await h.until((x) => [...x.texts].reverse().find((m) => m.t === 'lobby' && m.players.length === 2));
  ok(!!lob, 'host sees lobby with 2 players');
  ok(!JSON.stringify(lob).includes('token'), 'lobby list leaks no tokens');
  {
    const HTTP = BASE.replace(/^ws/, 'http');
    const pk = await fetch(HTTP + '/peek/' + w.code, { headers: { Origin: ORIGIN } });
    const pj = await pk.json().catch(() => null);
    ok(pk.status === 200 && pk.headers.get('access-control-allow-origin') === ORIGIN && pj && pj.open === true && pj.leader === 'HostPlayer' && pj.size === 2 && !JSON.stringify(pj).includes('token'), 'peek shows the open party, its leader and size (CORS for the page)');
    const pn = await fetch(HTTP + '/peek/ZZZZZZ', { headers: { Origin: ORIGIN } }).then((r) => r.json()).catch(() => null);
    ok(pn && pn.open === false, 'peek of an unknown code is closed');
    const pb = await fetch(HTTP + '/peek/' + w.code, { headers: { Origin: 'https://evil.example' } });
    ok(pb.status === 403, 'peek from a foreign origin rejected');
  }
  ok(lob.players.every((q) => q.ready === false && q.outfit === ''), 'members start not ready with no outfit');
  a.send({ t: 'member', ready: true, outfit: '/Game/Athena/Heroes/HID_005_Athena_Commando_M.HID_005_Athena_Commando_M' });
  const rl = await h.until((x) => [...x.texts].reverse().find((m) => m.t === 'lobby' && m.players.some((q) => q.slot === 1 && q.ready)));
  ok(rl && rl.players.find((q) => q.slot === 1).outfit.endsWith('HID_005_Athena_Commando_M'), 'member ready + outfit reach the host');
  const nLobby = h.texts.filter((m) => m.t === 'lobby').length;
  a.send({ t: 'member', ready: true, outfit: '../../etc/passwd' });
  await sleep(200);
  ok(h.texts.filter((m) => m.t === 'lobby').length === nLobby, 'bad outfit path rejected');
  h.send({ t: 'member', ready: false, playlist: 1 });
  const pl = await a.until((x) => [...x.texts].reverse().find((m) => m.t === 'lobby' && m.players.some((q) => q.slot === 0 && q.playlist === 1)));
  ok(!!pl, 'leader playlist reaches members');
  a.send({ t: 'member', ready: false });
  const ur = await h.until((x) => x.texts.filter((m) => m.t === 'lobby').slice(nLobby).find((m) => m.players.some((q) => q.slot === 1 && !q.ready)));
  ok(ur && ur.players.find((q) => q.slot === 1).outfit.endsWith('HID_005_Athena_Commando_M'), 'unready keeps the outfit');

  const others = [];
  for (let i = 0; i < 14; i++) others.push(await join(w.code, 'Bot' + i));
  ok(others.every((c) => c.texts.some((m) => m.t === 'welcome')), '16 players fit');
  const full = await join(w.code, 'Extra');
  ok(full.texts.some((m) => m.t === 'error' && m.code === 'full'), '17th join rejected (full)');
  for (const c of others) c.close();
  await sleep(300);

  a.send({ t: 'start' });
  await a.until((x) => x.closed !== null || false, 300);
  h.send({ t: 'start' });
  const st = await a.text('start');
  ok(!!st, 'host start reaches clients');

  const t0 = Date.now();
  a.send(state(1, 0, 10, 2, 10));
  const rel = await h.until((x) => x.bins.find((b) => b[0] === 0x01 && b[1] === 1));
  ok(rel && rel.length === 46, 'state relayed with sender slot stamped');
  const forged = new Uint8Array([0x40, 1, 1, 100, 100, 0]);
  a.send(forged);
  await sleep(200);
  ok(!h.bins.some((b) => b[0] === 0x40 && b[1] === 1), 'client cannot send server-only HEALTH');

  h.send(state(1, 0, 0, 2, 0));
  await sleep(120);
  a.send(state(2, 100, 10.3, 2, 10));
  await sleep(120);
  a.send(state(3, 200, 400, 2, 10));
  const cor = await a.until((x) => x.bins.find((b) => b[0] === 0x80));
  ok(!!cor, 'teleport rejected with CORRECT');
  const relayedTeleport = h.bins.some((b) => b[0] === 0x01 && b[1] === 1 && new DataView(b.buffer, b.byteOffset).getFloat32(9, true) > 300);
  ok(!relayedTeleport, 'teleport state not relayed');

  {
    const r = await join(w.code, 'Runner');
    let corrected = false;
    for (let i = 0; i < 40 && !corrected; i++) {
      r.send(state(i, i * 50, 50 + i * 0.75, 2, 50));
      await sleep(50);
      corrected = r.bins.some((b) => b[0] === 0x80);
    }
    ok(corrected, 'sustained 15 m/s rejected by the 1 s speed window');
    const w2 = await join(w.code, 'Walker');
    for (let i = 0; i < 30; i++) { w2.send(state(i, i * 50, 80 + i * 0.27, 2, 80)); await sleep(50); }
    ok(!w2.bins.some((b) => b[0] === 0x80), 'sprint speed (5.4 m/s) accepted');
    const bus = await join(w.code, 'BusRider');
    bus.send(state(1, 0, 100, 2, 100)); await sleep(80);
    bus.send(state(2, 60, 900, 800, -400, 0, 0, 0, 0x8000)); await sleep(80);
    for (let i = 0; i < 20; i++) { bus.send(state(3 + i, 100 + i * 50, 900 + i * 4, 800 - i * 2, -400, 0, 0, 0, 0x8000)); await sleep(50); }
    bus.send(state(30, 1200, 980, 30, -400)); await sleep(80);
    for (let i = 0; i < 10; i++) { bus.send(state(31 + i, 1250 + i * 50, 980 + i * 0.2, 30, -400)); await sleep(50); }
    ok(!bus.bins.some((b) => b[0] === 0x80), 'bus / skydive states (aerial flag 0x8000): boarding, flight and landing are not corrected');
    bus.send(state(50, 2000, 1500, 30, -400)); await sleep(150);
    ok(bus.bins.some((b) => b[0] === 0x80), 'on the ground again the normal teleport check applies');
    r.close(); w2.close(); bus.close();
  }
  const full0 = await h.until((x) => lastHealth(x, 0));
  ok(full0 && full0.hp === 100 && full0.sh === 0, 'start broadcasts full vitals from the server');
  h.send(heal(100, 50));
  const healed = await a.until((x) => { const v = lastHealth(x, 0); return v && v.sh === 50 ? v : null; });
  ok(!!healed, 'HEAL claim applied by the server');
  a.send(fire(1, 1));
  await sleep(100);
  a.send(hit(1, 0, 1, 500));
  await sleep(200);
  ok(!dmgs(h).length, 'over-cap damage claim dropped');
  a.send(fire(2, 1));
  await sleep(100);
  a.send(hit(2, 0, 1, 30));
  await h.until((x) => dmgs(x).length >= 1);
  let d = dmgs(h);
  ok(d.length === 1 && d[0].from === 0xff && d[0].victim === 0 && d[0].attacker === 1 && d[0].flags === 1 && d[0].amount === 30, 'server applies a valid hit to the shield first');
  const v1 = await h.until((x) => { const v = lastHealth(x, 0); return v && v.sh === 20 ? v : null; });
  ok(v1 && v1.hp === 100, 'HEALTH after shield hit = 100 / 20');
  a.send(hit(77, 0, 1, 10));
  await sleep(150);
  ok(dmgs(h).length === 1, 'hit without a fire dropped');
  a.send(hit(2, 0, 1, 10));
  await sleep(150);
  ok(dmgs(h).length === 1, 'second hit on the same fire dropped');
  a.send(fire(3, 1));
  await sleep(150);
  a.send(hitHead(3, 0, 1, 80));
  await h.until((x) => dmgs(x).length >= 3);
  d = dmgs(h);
  ok(d.length === 3 && d[1].flags === 3 && d[1].amount === 20 && d[2].flags === 2 && d[2].amount === 60, 'headshot overflow: shield 20 then health 60, head flag set');
  ok((await a.until((x) => dmgs(x).length >= 3)) && dmgs(a).length === 3, 'the shooter gets the DAMAGE events too');
  a.send(fire(4, 1));
  await sleep(150);
  a.send(hit(4, 0, 1, 80));
  const elim = await a.until((x) => x.bins.find((b) => b[0] === 0x41));
  ok(elim && elim[1] === 0xff && elim[2] === 0 && elim[3] === 1 && elim[4] === 1, 'lethal hit -> ELIM victim 0 killer 1 weapon 1');
  const dead = await a.until((x) => { const v = lastHealth(x, 0); return v && v.dead ? v : null; });
  ok(!!dead, 'HEALTH marks the victim dead');
  const fired = a.bins.filter((b) => b[0] === 0x02 && b[1] === 0).length;
  h.send(fire(9, 1));
  await sleep(150);
  ok(a.bins.filter((b) => b[0] === 0x02 && b[1] === 0).length === fired, 'a dead player cannot fire');
  const sp = await h.until((x) => x.bins.find((b) => b[0] === 0x43 && b[2] === 0), 6500);
  ok(!sp, 'no respawn: the eliminated player gets no SPAWN within 6.5 s (Battle Royale)');
  const still = lastHealth(h, 0);
  ok(still && still.dead && still.hp === 0, 'the eliminated player stays dead');

  let relayedFires = 0;
  const before = h.bins.filter((b) => b[0] === 0x02).length;
  const g = await join(w.code, 'Gunner');
  for (let i = 0; i < 30; i++) g.send(fire(100 + i, 40));
  await sleep(300);
  relayedFires = h.bins.filter((b) => b[0] === 0x02).length - before;
  g.close();
  ok(relayedFires < 6, `fire-rate cap on a 0.33/s sniper (${relayedFires} of 30 relayed)`);

  const big = new Uint8Array(400); big[0] = 0x04;
  a.send(big);
  await sleep(100);
  ok(!h.bins.some((b) => b[0] === 0x04 && b.length > 300), 'oversized message dropped');

  const nHealth = a.bins.filter((b) => b[0] === 0x40 && b[1] === 0).length;
  h.send(health(1, 50, 0));
  await sleep(200);
  ok(a.bins.filter((b) => b[0] === 0x40 && b[1] === 0).length === nHealth, 'even the host cannot forge HEALTH');

  const spam = await join(w.code, 'Spammer');
  for (let i = 0; i < 2000 && spam.closed === null; i++) spam.send(state(i, i, 0, 2, 0));
  await spam.until((x) => x.closed !== null, 4000);
  ok(spam.closed === 4003, `rate-limit violator kicked (close ${spam.closed})`);

  const k = await join(w.code, 'Kickme');
  const kw = k.texts.find((m) => m.t === 'welcome');
  h.send({ t: 'kick', slot: kw.slot });
  await k.until((x) => x.closed !== null);
  ok(k.closed === 4003, 'host kick closes the socket (4003)');

  h.close();
  const end = await a.text('end');
  ok(end && end.reason === 'host_left', 'host leaving ends the match');
  await a.until((x) => x.closed !== null);
  const after = await join(w.code, 'Late');
  ok(after.texts.some((m) => m.t === 'error' && m.code === 'no_lobby'), 'lobby closed after host left');

  console.log(failures ? `${failures} FAILED` : 'ALL PASS');
  process.exit(failures ? 1 : 0);
}
main().catch((e) => { console.error(e); process.exit(2); });
