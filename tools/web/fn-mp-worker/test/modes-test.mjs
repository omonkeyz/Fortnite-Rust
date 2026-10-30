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
const revive = (target, on) => new Uint8Array([0x08, target, on ? 1 : 0]);
const AR = 1;
const dmgs = (c) => c.bins.filter((b) => b[0] === 0x45).map((b) => ({ victim: b[2], attacker: b[3], flags: b[4], amount: new DataView(b.buffer, b.byteOffset).getFloat32(5, true) }));
const dbnos = (c) => c.bins.filter((b) => b[0] === 0x46).map((b) => ({ victim: b[2], state: b[3], by: b[4], weapon: b[5] }));
const elims = (c) => c.bins.filter((b) => b[0] === 0x41).map((b) => ({ victim: b[2], killer: b[3], weapon: b[4], head: b[5], flags: b[6] }));
const lastHealth = (c, slot) => { const hs = c.bins.filter((b) => b[0] === 0x40); for (let k = hs.length - 1; k >= 0; k--) { const b = hs[k]; for (let i = 0; i < b[2]; i++) if (b[3 + i * 4] === slot) return { hp: b[4 + i * 4], sh: b[5 + i * 4], dead: b[6 + i * 4] === 1 }; } return null; };

async function party(names) {
  const h = new Client('/create'); await h.open; h.send({ t: 'hello', name: names[0] });
  const w = await h.text('welcome');
  const all = [h];
  for (const n of names.slice(1)) { const c = new Client('/join/' + w.code); await c.open; c.send({ t: 'hello', name: n }); await c.text('welcome'); all.push(c); }
  await h.until((x) => [...x.texts].reverse().find((m) => m.t === 'lobby' && m.players.length === names.length));
  return { code: w.code, members: all };
}

async function toMatch(members) {
  const out = [];
  for (const c of members) {
    const a = await c.text('assign', 15000);
    const m = new Client('/match/' + a.match); await m.open; m.send({ t: 'hello', name: 'Player' + out.length, ticket: a.ticket });
    out.push({ m, assign: a });
  }
  for (const o of out) { o.welcome = await o.m.text("welcome"); if (!o.welcome) console.log("NO WELCOME", JSON.stringify(o.m.texts), o.m.closed); }
  return out;
}

let seq = 100;
async function shoot(from, target, damage) { seq++; from.send(fire(seq, AR)); await sleep(60); from.send(hit(seq, target, AR, damage)); await sleep(140); }

async function main() {
  {
    const big = await party(['Big0', 'Big1', 'Big2']);
    big.members[1].send({ t: 'queue', playlist: 2 });
    await sleep(200);
    ok(!big.members[1].texts.some((m) => m.t === 'queued'), 'only the party leader can queue');
    big.members[0].send({ t: 'queue', playlist: 1, fill: false });
    ok(!(await big.members[0].text('queue_error', 1500)), 'a party of 3 may queue Duo (port-defined: parties up to 16)');
    const bm = await toMatch(big.members);
    ok(new Set(bm.map((o) => o.assign.match)).size === 1, 'the whole party of 3 goes into one Duo match');
    const bt = bm.map((o) => o.welcome.team);
    ok(bt[0] === bt[1] && bt[2] !== bt[0], 'Duo teams follow the join order: members 1+2 a pair, member 3 the next team (' + bt.join(',') + ')');
    big.members.forEach((c) => c.close()); bm.forEach((o) => o.m.close());
  }
  {
    const solo = await party(['Solo0', 'Solo1', 'Solo2', 'Solo3', 'Solo4']);
    solo.members[0].send({ t: 'queue', playlist: 0 });
    ok(!(await solo.members[0].text('queue_error', 1500)), 'a party of 5 may queue Solo');
    const sm = await toMatch(solo.members);
    ok(new Set(sm.map((o) => o.assign.match)).size === 1 && new Set(sm.map((o) => o.welcome.team)).size === 5, 'Solo: the party of 5 shares a match, everyone on their own team');
    solo.members.forEach((c) => c.close()); sm.forEach((o) => o.m.close());
  }
  {
    const names = Array.from({ length: 16 }, (_, i) => 'Squad' + i);
    const sq = await party(names);
    ok(sq.members.length === 16, 'a party holds 16 members');
    const extra = new Client('/join/' + sq.code); await extra.open; extra.send({ t: 'hello', name: 'Squad16' });
    await extra.until((c) => c.closed !== null || c.texts.some((m) => m.t === 'welcome'), 3000);
    ok(!extra.texts.some((m) => m.t === 'welcome'), 'a 17th member is refused (party full at 16)');
    extra.close();
    sq.members[0].send({ t: 'queue', playlist: 2, fill: false });
    ok(!(await sq.members[0].text('queue_error', 1500)), 'a party of 16 may queue Squad');
    const qm = await toMatch(sq.members);
    const qt = qm.map((o) => o.welcome.team);
    const groups = [0, 4, 8, 12].map((k) => qt.slice(k, k + 4));
    ok(new Set(qm.map((o) => o.assign.match)).size === 1 && groups.every((g) => new Set(g).size === 1) && new Set(groups.map((g) => g[0])).size === 4, 'Squad: 16 members become 4 squads of 4 in join order (' + qt.join(',') + ')');
    sq.members.forEach((c) => c.close()); qm.forEach((o) => o.m.close());
  }

  const A = await party(['Alpha', 'Bravo']);
  const B = await party(['Enemy']);
  A.members[0].send({ t: 'queue', playlist: 1, fill: false });
  ok(!!(await A.members[1].text('queued', 2000)), 'the Duo party is queued (members told)');
  B.members[0].send({ t: 'queue', playlist: 1 });
  const ms = await toMatch([...A.members, ...B.members]);
  const [a1, a2, b1] = ms;
  ok(a1.assign.match === a2.assign.match && a1.assign.match === b1.assign.match, 'both parties are matched into one match');
  ok(a1.welcome.team === a2.welcome.team && a1.welcome.team !== b1.welcome.team, 'the party is one team, the other party another');
  ok(a1.welcome.playlist === 1 && a1.welcome.match === true, 'welcome carries the Duo playlist');
  ok(a1.welcome.host === true && !a2.welcome.host && !b1.welcome.host, 'the first party leader hosts the match');
  const st = await b1.m.text('start', 3000);
  ok(st && st.playlist === 1 && st.squad === true, 'match starts when every ticket joined (Duo = squad play)');
  const lob = [...a1.m.texts].reverse().find((m) => m.t === 'lobby');
  ok(lob && lob.players.every((p) => typeof p.team === 'number'), 'match lobby lists the team of every player');
  const S = (o) => o.welcome.slot;
  a1.m.send(state(1, 0, 2, 0)); a2.m.send(state(1, 1, 2, 0)); b1.m.send(state(1, 3, 2, 0));
  await sleep(200);

  await shoot(a2.m, S(a1), 50);
  ok(!dmgs(a1.m).length, 'friendly fire: a rifle hit on a teammate does no damage');

  await shoot(b1.m, S(a1), 60);
  await shoot(b1.m, S(a1), 60);
  const down = await a2.m.until((x) => dbnos(x).find((d) => d.state === 1));
  ok(down && down.victim === S(a1) && down.by === S(b1) && down.weapon === AR, 'lethal damage with a standing teammate knocks down (DBNO, knocker + weapon)');
  const hv = await a1.m.until((x) => { const v = lastHealth(x, S(a1)); return v && v.hp === 100 ? v : null; });
  ok(hv && !hv.dead && hv.sh === 0, 'knocked player has Default.DBNO.InitialHealth 100, no shield, not dead');
  const n0 = dmgs(a1.m).length;
  await shoot(b1.m, S(a1), 30);
  ok(dmgs(a1.m).length === n0, 'DBNOInvulnerableTime 2 s: no damage right after the knock');
  const f0 = a2.m.bins.filter((b) => b[0] === 0x02 && b[1] === S(a1)).length;
  a1.m.send(fire(500, AR)); await sleep(150);
  ok(a2.m.bins.filter((b) => b[0] === 0x02 && b[1] === S(a1)).length === f0, 'a knocked player cannot fire');
  const bleed = await a1.m.until((x) => dmgs(x).find((d) => d.victim === S(a1) && d.flags === 4 && d.attacker === 0xff), 3500);
  ok(bleed && bleed.amount === 2, 'bleed: Default.DBNO.BleedDamage 2 per Default.DBNO.BleedTick 2 s');

  a2.m.send(revive(S(a1), true));
  const rs = await a1.m.until((x) => dbnos(x).find((d) => d.state === 2));
  ok(rs && rs.by === S(a2), 'teammate starts the revive');
  b1.m.send(revive(S(a1), true)); await sleep(150);
  ok(dbnos(a1.m).filter((d) => d.state === 2).length === 1, 'an enemy cannot revive');
  const before = dmgs(a1.m).filter((d) => d.flags === 4 && d.attacker === 0xff).length;
  const rev = await a1.m.until((x) => dbnos(x).find((d) => d.state === 0), 11500);
  ok(rev && rev.by === S(a2), 'revive completes after ReviveFromDBNOTime 10 s');
  ok(dmgs(a1.m).filter((d) => d.flags === 4 && d.attacker === 0xff).length === before, 'no bleed while being revived (DBNOBeingRevived)');
  const hr = await a1.m.until((x) => { const v = lastHealth(x, S(a1)); return v && v.hp === 30 ? v : null; });
  ok(!!hr, 'revived with Default.DBNO.ReviveHealth 30');

  await shoot(b1.m, S(a1), 60);
  ok(!!(await a1.m.until((x) => dbnos(x).filter((d) => d.state === 1).length === 2)), 'second knock');
  await sleep(2100);
  await shoot(b1.m, S(a1), 80);
  await shoot(b1.m, S(a1), 30);
  const fin = await a2.m.until((x) => elims(x).find((e) => e.victim === S(a1)));
  ok(fin && fin.killer === S(b1) && (fin.flags & 1) === 1, 'a knocked player finished off is eliminated (ELIM flag WAS_DBNO)');
  await shoot(b1.m, S(a2), 80);
  await shoot(b1.m, S(a2), 80);
  const last = await a2.m.until((x) => elims(x).find((e) => e.victim === S(a2)));
  ok(last && !dbnos(a2.m).some((d) => d.victim === S(a2) && d.state === 1), 'the last standing teammate is eliminated without DBNO');
  await sleep(6000);
  ok(!a1.m.bins.some((b) => b[0] === 0x43), 'no respawn in the match');
  ms.forEach((o) => o.m.close()); [...A.members, ...B.members].forEach((c) => c.close());

  const D = await party(['Delta', 'Echo']);
  const E = await party(['Foxtrot']);
  D.members[0].send({ t: 'queue', playlist: 1 });
  E.members[0].send({ t: 'queue', playlist: 1 });
  const ms2 = await toMatch([...D.members, ...E.members]);
  const [d1, d2, e1] = ms2;
  await e1.m.text('start', 3000);
  d1.m.send(state(1, 0, 2, 0)); d2.m.send(state(1, 1, 2, 0)); e1.m.send(state(1, 3, 2, 0));
  await sleep(200);
  await shoot(e1.m, S(d1), 60); await shoot(e1.m, S(d1), 60);
  ok(!!(await d1.m.until((x) => dbnos(x).find((d) => d.state === 1 && d.victim === S(d1)))), 'Delta knocked');
  d2.m.send(revive(S(d1), true));
  await d1.m.until((x) => dbnos(x).find((d) => d.state === 2));
  await shoot(e1.m, S(d2), 60); await shoot(e1.m, S(d2), 60);
  const wipe = await d1.m.until((x) => { const l = elims(x); return l.length >= 2 ? l : null; });
  ok(wipe && wipe.some((e) => e.victim === S(d2)) && wipe.some((e) => e.victim === S(d1) && (e.flags & 4) === 4 && e.killer === S(e1)), 'team wipe: the last standing member down eliminates the knocked teammate too (credited to its knocker)');
  ok(dbnos(d1.m).some((d) => d.state === 3 && d.victim === S(d1)), 'the revive stops when the reviver goes down');
  ok(lastHealth(d1.m, S(d1)).dead && lastHealth(d1.m, S(d2)).dead && !lastHealth(d1.m, S(e1)).dead, 'whole team eliminated, the enemy survives');
  ms2.forEach((o) => o.m.close()); [...D.members, ...E.members].forEach((c) => c.close());

  {
    const S1 = await party(['Solo1']);
    const S2 = await party(['Solo2']);
    S1.members[0].send({ t: 'queue', playlist: 0 });
    S2.members[0].send({ t: 'queue', playlist: 0 });
    const [s1, s2] = await toMatch([...S1.members, ...S2.members]);
    await s1.m.text('start', 3000);
    ok(s1.welcome.team !== s2.welcome.team, 'Solo: every player its own team');
    s1.m.send(state(1, 0, 2, 0)); s2.m.send(state(1, 2, 2, 0)); await sleep(200);
    await shoot(s2.m, S(s1), 60); await shoot(s2.m, S(s1), 60);
    const el = await s1.m.until((x) => elims(x).find((e) => e.victim === S(s1)));
    ok(el && !dbnos(s1.m).length, 'Solo: lethal damage eliminates, no DBNO');
    [s1, s2].forEach((o) => o.m.close()); [...S1.members, ...S2.members].forEach((c) => c.close());
  }
  {
    const bad = new Client('/match/ZZZZZ2'); await bad.open; bad.send({ t: 'hello', name: 'Nobody', ticket: 'x' });
    await bad.until((x) => x.texts.find((m) => m.t === 'error') || x.closed);
    ok(bad.texts.some((m) => m.t === 'error'), 'joining a match without a valid ticket is refused');
  }
  console.log(failures ? `${failures} FAILED` : 'ALL PASS');
  process.exit(failures ? 1 : 0);
}
main().catch((e) => { console.error(e); process.exit(2); });
