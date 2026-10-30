import { chromium } from 'playwright-core';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { BRAVE } from './lib.mjs';

const argv = process.argv.slice(2);
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 ? argv[i + 1] : d; };
const has = (n) => argv.includes(n);
const www = opt('--www', 'D:/fn-mp-web');
const exe = opt('--exe', 'dist/fortnite-web.exe');
const out = opt('--out', 'D:/fn-verify/mp/combat');
const port = Number(opt('--port', '8804'));
const mpUrl = opt('--mp', 'ws://127.0.0.1:8805');
const extra = opt('--page-args', 'FN_BUS=0&FN_DISPLAY_NAME=Tester');
const steps = (opt('--steps', 'fire,elim,melee')).split(',');
fs.mkdirSync(out, { recursive: true });
const t0 = Date.now();
const log = (m) => { const l = `[${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}] ${m}`; console.log(l); fs.appendFileSync(path.join(out, 'driver.log'), l + '\n'); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const server = spawn(exe, ['--no-browser', '--quiet', '--port', String(port), '--www', www], { stdio: ['ignore', 'pipe', 'pipe'] });
let serverOut = '';
server.stdout.on('data', (d) => { serverOut += d; });
server.stderr.on('data', (d) => { serverOut += d; });
while (!/127\.0\.0\.1:\d+/.test(serverOut)) { if (server.exitCode !== null) throw new Error(serverOut); await sleep(100); }
const url = `http://127.0.0.1:${port}/?fnweb_fullscreen=off&FN_LOBBY=0&fnweb_mp=1&fnweb_mp_url=${encodeURIComponent(mpUrl)}${extra ? '&' + extra : ''}`;
log('server ' + url);

async function open(name, x) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'fnweb-mp-'));
  const ctx = await chromium.launchPersistentContext(profile, {
    executablePath: BRAVE, headless: false, viewport: null,
    args: ['--no-first-run', '--no-default-browser-check', '--window-size=1270,760', `--window-position=${x},40`, '--enable-unsafe-webgpu', '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows'],
  });
  const page = ctx.pages()[0] || await ctx.newPage();
  const file = path.join(out, name + '.log');
  fs.writeFileSync(file, '');
  const tag = (src) => (m) => fs.appendFileSync(file, `[${((Date.now() - t0) / 1000).toFixed(1)}][${src}] ${m.text()}\n`);
  page.on('console', tag('page'));
  page.on('pageerror', (e) => fs.appendFileSync(file, '[exception] ' + (e.stack || e) + '\n'));
  page.on('worker', (w) => w.on('console', tag('worker')));
  await page.goto(url);
  return { name, ctx, page, profile, file };
}
const grep = (c, re) => fs.readFileSync(c.file, 'utf8').split('\n').filter((l) => re.test(l));
async function shot(c, tag) { try { await c.page.screenshot({ path: path.join(out, `${c.name}-${tag}.png`), timeout: 15000 }); } catch (e) { log(c.name + ' screenshot ' + tag + ' failed'); } }
const focus = async (c) => { await c.page.bringToFront(); await c.page.mouse.click(640, 380); await sleep(400); };
const consoleCmd = async (c, cmd) => { await c.page.keyboard.press('Backquote'); await sleep(400); await c.page.keyboard.type(cmd, { delay: 20 }); await c.page.keyboard.press('Enter'); await sleep(400); await c.page.keyboard.press('Backquote'); await sleep(400); };
const burst = async (c, ms) => { await c.page.mouse.down(); await sleep(ms); await c.page.mouse.up(); };

let status = 1;
const clients = [];
try {
  const host = await open('host', 0); clients.push(host);
  const guest = await open('guest', 1280); clients.push(guest);
  for (const c of clients) {
    for (let k = 0; k < 600 && !(await c.page.evaluate(() => !!window.__fnwebReady).catch(() => false)); k++) await sleep(500);
    await c.page.mouse.click(200, 200);
  }
  await host.page.evaluate(() => globalThis.__fnweb_mp_create('HostOne'));
  await host.page.waitForFunction(() => /^[A-Z2-9]{6}$/.test(globalThis.__fnwebMp.state.code || ''), null, { timeout: 30000 });
  const code = await host.page.evaluate(() => globalThis.__fnwebMp.state.code);
  log('lobby ' + code);
  await guest.page.evaluate((c) => globalThis.__fnweb_mp_join(c, 'GuestTwo'), code);
  await guest.page.waitForFunction(() => globalThis.__fnwebMp.state.slot >= 0, null, { timeout: 30000 });
  log('both in lobby');
  await host.page.waitForFunction(() => ['stage:GamePicker', 'stage:Lobby'].includes(window.__fnwebPhase), null, { timeout: 900000 }).catch(() => log('host picker wait timed out'));
  await guest.page.waitForFunction(() => ['stage:GamePicker', 'stage:Lobby'].includes(window.__fnwebPhase), null, { timeout: 900000 }).catch(() => log('guest picker wait timed out'));
  await host.page.evaluate(() => globalThis.__fnweb_mp_text_send(JSON.stringify({ t: 'start' })));
  for (const c of clients) await c.page.waitForFunction(() => window.__fnwebPhase === 'playing', null, { timeout: 900000 });
  log('both playing');
  await sleep(10000);
  await shot(host, 'spawn'); await shot(guest, 'spawn');
  for (const c of clients) log(c.name + ' sees: ' + grep(c, /\[mp\] remote|hit bodies/).join(' | '));
  for (const c of clients) { await focus(c); await consoleCmd(c, 'giveathenafullkit'); }
  if (steps.includes('fire')) {
    await focus(guest);
    await guest.page.keyboard.press('Digit2');
    await sleep(1500);
    await burst(guest, 120);
    await sleep(250);
    await shot(guest, 'first-hit');
    await sleep(1500);
    log('guest hits: ' + grep(guest, /\[mp\] (hit|hit claim)/).slice(0, 6).join(' | '));
    log('host took: ' + grep(host, /\[mp\] (took|own vitals)/).slice(0, 6).join(' | '));
    await shot(host, 'after-first-hit');
  }
  if (steps.includes('elim')) {
    for (let i = 0; i < 12 && !grep(host, /\[mp\] elimination/).length; i++) { await focus(guest); await burst(guest, 700); await sleep(900); }
    await sleep(500);
    await shot(guest, 'elim'); await shot(host, 'elim');
    log('host elim: ' + grep(host, /\[mp\] elimination|\[killfeed\]/).slice(0, 4).join(' | '));
    log('guest elim: ' + grep(guest, /\[mp\] elimination|\[killfeed\]/).slice(0, 4).join(' | '));
    await sleep(6500);
    log('host respawn: ' + grep(host, /\[mp\] respawned/).join(' | '));
    await shot(host, 'respawned'); await shot(guest, 'respawned');
  }
  if (steps.includes('melee')) {
    await focus(host);
    await host.page.keyboard.press('Digit1');
    await sleep(800);
    for (let i = 0; i < 4; i++) { await burst(host, 300); await sleep(900); }
    await sleep(1000);
    log('melee host: ' + grep(host, /pickaxe swing/).slice(0, 4).join(' | '));
    log('melee guest took: ' + grep(guest, /\[mp\] took/).slice(0, 4).join(' | '));
    await shot(host, 'melee'); await shot(guest, 'melee');
  }
  for (const c of clients) {
    const errs = grep(c, /exception|panicked|The game stopped/);
    if (errs.length) log(c.name + ' errors: ' + errs.slice(0, 5).join(' | '));
  }
  if (has('--hold')) { log('holding windows open 600 s'); await sleep(600000); }
  status = 0;
} catch (e) {
  log('FAILED ' + (e.stack || e));
  for (const c of clients) { try { await shot(c, 'fail'); } catch (_) {} }
} finally {
  for (const c of clients) { try { await c.ctx.close(); } catch (_) {} try { fs.rmSync(c.profile, { recursive: true, force: true }); } catch (_) {} }
  try { server.kill(); } catch (_) {}
  log('done status ' + status);
  process.exit(status);
}
