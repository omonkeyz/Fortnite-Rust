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
async function party(names) {
  const h = new Client('/create'); await h.open; h.send({ t: 'hello', name: names[0] });
  const w = await h.text('welcome');
  const all = [h];
  for (const n of names.slice(1)) { const c = new Client('/join/' + w.code); await c.open; c.send({ t: 'hello', name: n }); await c.text('welcome'); all.push(c); }
  await h.until((x) => [...x.texts].reverse().find((m) => m.t === 'lobby' && m.players.length === names.length));
  return { code: w.code, members: all };
}

const emote = (op, item) => new Uint8Array([0x0c, op, item]);
const emotes = (c) => c.bins.filter((b) => b[0] === 0x0c).map((b) => ({ from: b[1], op: b[2], item: b[3], len: b.length }));

async function main() {
  const P = await party(['Host', 'Second', 'Third']);
  const [a, b, c] = P.members;
  const slot = (x) => x.texts.find((m) => m.t === 'welcome').slot;
  const sa = slot(a);
  a.send(emote(1, 0));
  await sleep(250);
  ok(!emotes(b).length, 'an emote before the match starts is not relayed');
  a.send({ t: 'start' });
  for (const x of [a, b, c]) await x.text('start', 3000);
  a.send(state(1, 0, 2, 0)); b.send(state(1, 2, 2, 0)); c.send(state(1, 30, 2, 0));
  await sleep(250);
  a.send(emote(1, 0));
  const e1 = await b.until((x) => emotes(x).find((e) => e.op === 1));
  ok(!!e1 && e1.from === sa && e1.item === 0 && e1.len === 4, 'a play is relayed to the others with the sender slot');
  ok(!!(await c.until((x) => emotes(x).find((e) => e.op === 1))), 'every other client gets the play');
  await sleep(150);
  ok(!emotes(a).length, 'the sender does not get its own emote back');
  a.send(emote(0, 0));
  ok(!!(await b.until((x) => emotes(x).find((e) => e.op === 0 && e.from === sa))), 'a stop (cancel) is relayed');
  const n = emotes(b).length;
  a.send(new Uint8Array([0x0c, 1]));
  a.send(emote(7, 0));
  await sleep(250);
  ok(emotes(b).length === n, 'a malformed or unknown-op emote is refused');
  for (let k = 0; k < 20; k++) c.send(emote(1, 0));
  await sleep(300);
  const got = emotes(a).filter((e) => e.from === slot(c)).length;
  ok(got > 0 && got <= 9, `emote spam is rate limited (${got} of 20 relayed)`);
  [a, b, c].forEach((x) => x.close());
  console.log(failures ? `${failures} FAILED` : 'ALL PASS');
  process.exit(failures ? 1 : 0);
}
main().catch((e) => { console.error(e); process.exit(2); });
