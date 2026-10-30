import WebSocket from 'ws';

const BASE = process.env.MP_URL || 'ws://127.0.0.1:8798';
const ORIGIN = process.env.MP_ORIGIN || 'http://127.0.0.1:8797';
let failures = 0;
const ok = (cond, what) => { console.log((cond ? 'PASS ' : 'FAIL ') + what); if (!cond) failures++; };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class Client {
  constructor(path) {
    this.texts = []; this.bins = []; this.closed = null;
    this.ws = new WebSocket(BASE + path, { origin: ORIGIN });
    this.ws.binaryType = 'arraybuffer';
    this.open = new Promise((res) => { this.ws.on('open', () => res(true)); this.ws.on('error', () => res(false)); });
    this.ws.on('message', (d, isBin) => { if (isBin) this.bins.push(new Uint8Array(d)); else this.texts.push(JSON.parse(d.toString())); });
    this.ws.on('close', (code) => { this.closed = code; });
  }
  async until(pred, ms = 3000) { const t0 = Date.now(); while (Date.now() - t0 < ms) { const v = pred(this); if (v) return v; await sleep(30); } return pred(this); }
  text(t, ms) { return this.until((c) => c.texts.find((m) => m.t === t), ms); }
  send(o) { this.ws.send(typeof o === 'string' || o instanceof Uint8Array ? o : JSON.stringify(o)); }
  close() { try { this.ws.close(); } catch (_) {} }
}
function state(seq, tms, x, y, z) {
  const b = new Uint8Array(45); const v = new DataView(b.buffer);
  b[0] = 0x01; v.setUint32(1, seq, true); v.setUint32(5, tms, true);
  [x, y, z, 0, 0, 0, 90, 0].forEach((f, i) => v.setFloat32(9 + i * 4, f, true));
  b[43] = 1;
  return b;
}
function fire(seq, weapon) {
  const b = new Uint8Array(31); const v = new DataView(b.buffer);
  b[0] = 0x02; v.setUint32(1, seq, true); b[5] = weapon;
  [0, 1.7, 0, 1, 0, 0].forEach((f, i) => v.setFloat32(7 + i * 4, f, true));
  return b;
}
function hit(fireSeq, target, weapon, damage, head = 0) {
  const b = new Uint8Array(13); const v = new DataView(b.buffer);
  b[0] = 0x03; v.setUint32(1, fireSeq, true); b[5] = target; b[6] = weapon; b[7] = head; b[8] = 1; v.setFloat32(9, damage, true);
  return b;
}
const dmgs = (c) => c.bins.filter((b) => b[0] === 0x45).map((b) => ({ victim: b[2], attacker: b[3], flags: b[4], amount: new DataView(b.buffer, b.byteOffset).getFloat32(5, true) }));
const lastHealth = (c, slot) => { const hs = c.bins.filter((b) => b[0] === 0x40 && b[1] === 0xff); for (let k = hs.length - 1; k >= 0; k--) { const b = hs[k]; for (let i = 0; i < b[2]; i++) if (b[3 + i * 4] === slot) return { hp: b[4 + i * 4], sh: b[5 + i * 4] }; } return null; };

async function main() {
  const h = new Client('/create'); await h.open; h.send({ t: 'hello', name: 'IslandHost' });
  const w = await h.text('welcome');
  const a = new Client('/join/' + w.code); await a.open; a.send({ t: 'hello', name: 'IslandGuest' });
  await a.text('welcome');
  await h.until((x) => x.texts.find((m) => m.t === 'lobby' && m.players.length === 2));
  h.send({ t: 'start' });
  const st = await a.text('start');
  ok(st && st.warmupMs > 0, `start carries the spawn-island warmup window (${st && st.warmupMs} ms)`);
  if (!(st && st.warmupMs > 0)) { console.log('relay runs with WARMUP_MS=0, nothing more to check'); h.close(); a.close(); process.exit(failures ? 1 : 0); }
  h.send(state(1, 0, 0, 2, 0)); a.send(state(1, 0, 3, 2, 0));
  await sleep(200);
  a.send(fire(1, 1)); await sleep(100);
  a.send(hit(1, 0, 1, 30, 1));
  const d = await h.until((x) => dmgs(x).length >= 1 ? dmgs(x) : null);
  ok(d && d.length === 1 && d[0].victim === 0 && d[0].attacker === 1 && d[0].amount === 0 && (d[0].flags & 8) === 8 && (d[0].flags & 2) === 2, 'island hit -> zero-amount DAMAGE with the warmup bit (victim hit-react, shooter marker)');
  ok(!!(await a.until((x) => dmgs(x).length >= 1)), 'the shooter receives the zero-damage event too');
  await sleep(300);
  const v = lastHealth(h, 0);
  ok(v && v.hp === 100 && v.sh === 0, 'victim vitals unchanged on the island');
  a.send(fire(2, 0)); await sleep(100);
  a.send(hit(2, 0, 0, 15));
  await sleep(300);
  ok(dmgs(h).length === 2 && dmgs(h)[1].amount === 0 && lastHealth(h, 0).hp === 100, 'pickaxe hit on the island does no damage either');
  h.close(); a.close();
  console.log(failures ? `${failures} FAILED` : 'all passed');
  process.exit(failures ? 1 : 0);
}
main();
