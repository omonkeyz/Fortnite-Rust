import WebSocket from 'ws';

const BASE = process.env.MP_URL || 'ws://127.0.0.1:8798';
const ORIGIN = process.env.MP_ORIGIN || 'http://127.0.0.1:8797';
let failures = 0;
const ok = (cond, what) => { console.log((cond ? 'PASS ' : 'FAIL ') + what); if (!cond) failures++; };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class Client {
  constructor(path) {
    this.texts = []; this.bins = []; this.closed = null; this.waiters = [];
    this.ws = new WebSocket(BASE + path, { origin: ORIGIN });
    this.ws.binaryType = 'arraybuffer';
    this.open = new Promise((res) => { this.ws.on('open', () => res(true)); this.ws.on('error', () => res(false)); });
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

function state(seq, x, y, z) {
  const b = new Uint8Array(45); const v = new DataView(b.buffer);
  b[0] = 0x01; v.setUint32(1, seq, true); v.setUint32(5, seq * 50, true);
  [x, y, z, 0, 0, 0, 90, 0].forEach((f, i) => v.setFloat32(9 + i * 4, f, true));
  return b;
}
function fire(seq, weapon) {
  const b = new Uint8Array(31); const v = new DataView(b.buffer);
  b[0] = 0x02; v.setUint32(1, seq, true); b[5] = weapon;
  [0, 1.7, 0, 1, 0, 0].forEach((f, i) => v.setFloat32(7 + i * 4, f, true));
  return b;
}
function hit(fireSeq, target, weapon, damage) {
  const b = new Uint8Array(13); const v = new DataView(b.buffer);
  b[0] = 0x03; v.setUint32(1, fireSeq, true); b[5] = target; b[6] = weapon; b[8] = 1; v.setFloat32(9, damage, true);
  return b;
}
function spawnReq(flags, item, count, start, end) {
  const b = new Uint8Array(29 + item.length); const v = new DataView(b.buffer);
  b[0] = 0x0a; b[1] = flags; v.setUint16(2, count, true);
  [...start, ...end].forEach((f, i) => v.setFloat32(4 + i * 4, f, true));
  b[28] = item.length; for (let i = 0; i < item.length; i++) b[29 + i] = item.charCodeAt(i);
  return b;
}
function claim(id) { const b = new Uint8Array(5); new DataView(b.buffer).setUint32(1, id, true); b[0] = 0x0b; return b; }
const pickups = (c) => c.bins.filter((b) => b[0] === 0x47).map((b) => { const v = new DataView(b.buffer, b.byteOffset); let item = ''; for (let i = 0; i < b[34]; i++) item += String.fromCharCode(b[35 + i]); return { id: v.getUint32(2, true), owner: b[6], flags: b[7], count: v.getUint16(8, true), item }; });
const gones = (c) => c.bins.filter((b) => b[0] === 0x48).map((b) => ({ id: new DataView(b.buffer, b.byteOffset).getUint32(2, true), taker: b[6] }));
const elims = (c) => c.bins.filter((b) => b[0] === 0x41).map((b) => ({ victim: b[2] }));
const AR = 1;

async function party(names) {
  const h = new Client('/create'); await h.open; h.send({ t: 'hello', name: names[0] });
  const w = await h.text('welcome');
  const all = [h];
  for (const n of names.slice(1)) { const c = new Client('/join/' + w.code); await c.open; c.send({ t: 'hello', name: n }); await c.text('welcome'); all.push(c); }
  await h.until((x) => [...x.texts].reverse().find((m) => m.t === 'lobby' && m.players.length === names.length));
  return { code: w.code, members: all };
}

async function main() {
  const P = await party(['Host', 'Second', 'Third']);
  const [a, b, c] = P.members;
  const slot = (x) => x.texts.find((m) => m.t === 'welcome').slot;
  const [sa, sb] = [a, b].map(slot);
  a.send({ t: 'start' });
  for (const x of [a, b, c]) await x.text('start', 3000);
  a.send(state(1, 0, 2, 0)); b.send(state(1, 2, 2, 0)); c.send(state(1, 30, 2, 0));
  await sleep(250);

  b.send(spawnReq(2, 'WID_Assault_Auto_Athena_C_Ore_T02', 1, [2, 2, 0], [2, 0.8, 5.12]));
  const p1 = await a.until((x) => pickups(x).find((p) => p.owner === sb && p.flags === 2));
  ok(!!p1 && p1.item === 'WID_Assault_Auto_Athena_C_Ore_T02' && p1.count === 1, 'a player drop is broadcast to everyone with an id');
  ok(!!(await b.until((x) => pickups(x).find((p) => p.id === p1?.id))), 'the dropper gets the broadcast too');
  ok(!!(await c.until((x) => pickups(x).find((p) => p.id === p1?.id))), 'a third client gets it');

  b.send(spawnReq(1, 'Athena_Bandage', 5, [2, 2, 0], [3, 0.8, 1]));
  await sleep(300);
  ok(!pickups(a).some((p) => p.flags & 1), 'a death drop from a living player is refused');

  b.send(spawnReq(2, 'Athena_Bandage', 5, [40, 2, 0], [41, 0.8, 0]));
  await sleep(300);
  ok(!pickups(a).some((p) => p.item === 'Athena_Bandage'), 'a drop far from the sender is refused');

  b.send(spawnReq(2, 'bad item!', 1, [2, 2, 0], [2, 0.8, 1]));
  await sleep(200);
  ok(pickups(a).length === 1, 'an item id outside the id charset is refused');

  c.send(claim(p1.id));
  await sleep(300);
  ok(!gones(a).length, 'a claim from too far away does nothing');
  a.send(claim(p1.id)); b.send(claim(p1.id));
  const g = await c.until((x) => gones(x).find((e) => e.id === p1.id));
  ok(!!g && (g.taker === sa || g.taker === sb), 'the first claim wins and the despawn is broadcast');
  await sleep(300);
  ok(gones(c).filter((e) => e.id === p1.id).length === 1, 'the second claim on the same item is ignored (one taker only)');
  ok(gones(a).length === 1 && gones(b).length === 1, 'every client sees exactly one despawn');

  let seq = 10;
  for (let k = 0; k < 2; k++) { seq++; a.send(fire(seq, AR)); await sleep(60); a.send(hit(seq, sb, AR, 60)); await sleep(200); }
  ok(!!(await b.until((x) => elims(x).find((e) => e.victim === sb))), 'the victim is eliminated (no squad play: no DBNO)');
  const items = ['WID_Shotgun_Standard_Athena_UC_Ore_T03', 'AthenaAmmoDataShells', 'WoodItemData', 'Athena_Shields'];
  for (const it of items) b.send(spawnReq(1, it, 3, [2, 2, 0], [2 + Math.random() * 2, 0.8, Math.random() * 2]));
  const deathDrops = await a.until((x) => { const d = pickups(x).filter((p) => p.flags === 1 && p.owner === sb); return d.length === items.length ? d : null; });
  ok(!!deathDrops, 'the eliminated player\'s inventory spills: every death drop is broadcast');
  ok(!!(await c.until((x) => pickups(x).filter((p) => p.flags === 1).length === items.length)), 'the other players see the same death drops');
  b.send(spawnReq(2, 'Athena_Bandage', 1, [2, 2, 0], [2, 0.8, 1]));
  await sleep(250);
  ok(!pickups(a).some((p) => p.item === 'Athena_Bandage'), 'a dead player cannot make a normal drop');
  if (deathDrops) {
    a.send(claim(deathDrops[0].id));
    ok(!!(await c.until((x) => gones(x).find((e) => e.id === deathDrops[0].id && e.taker === sa))), 'the killer picks up a death drop and everyone agrees');
    b.send(claim(deathDrops[1].id));
    await sleep(300);
    ok(!gones(a).some((e) => e.id === deathDrops[1].id), 'a dead player cannot claim');
  }
  [a, b, c].forEach((x) => x.close());

  const Q = await party(['Queuer']);
  const q = Q.members[0];
  q.send({ t: 'queue', playlist: 0 });
  ok(!!(await q.text('queued', 2000)), 'the party is queued');
  q.send({ t: 'unqueue' });
  ok(!!(await q.text('unqueued', 2000)), 'unqueue cancels matchmaking (unqueued)');
  ok(!!(await q.until((x) => [...x.texts].reverse().find((m) => m.t === 'lobby' && m.queued === false))), 'the lobby is no longer queued');
  await sleep(Number(process.env.MM_WAIT_MS || 11500));
  ok(!q.texts.some((m) => m.t === 'assign'), 'no match is assigned after the cancel');
  q.send({ t: 'queue', playlist: 0 });
  ok(!!(await q.until((x) => x.texts.filter((m) => m.t === 'queued').length === 2, 2000)), 'the party can queue again after cancelling');
  q.send({ t: 'unqueue' });
  await q.text('unqueued', 2000);
  q.close();

  console.log(failures ? `${failures} FAILED` : 'ALL PASS');
  process.exit(failures ? 1 : 0);
}
main().catch((e) => { console.error(e); process.exit(2); });
