import { chromium } from 'playwright-core';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { BRAVE } from './lib.mjs';

const argv = process.argv.slice(2);
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 ? argv[i + 1] : d; };
const www = opt('--www', 'D:/fn-site-mp');
const exe = opt('--exe', 'D:/fn-verify/mp/fortnite-web.exe');
const out = opt('--out', 'D:/fn-verify/mp/run');
const port = Number(opt('--port', '8797'));
const extra = opt('--page-args', 'fnweb_mp=1') + '&FN_LOBBY=0';
fs.mkdirSync(out, { recursive: true });
const t0 = Date.now();
const log = (m) => { const l = `[${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}] ${m}`; console.log(l); fs.appendFileSync(path.join(out, 'driver.log'), l + '\n'); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const server = spawn(exe, ['--no-browser', '--quiet', '--port', String(port), '--www', www], { stdio: ['ignore', 'pipe', 'pipe'] });
let serverOut = '';
server.stdout.on('data', (d) => { serverOut += d; });
server.stderr.on('data', (d) => { serverOut += d; });
fs.writeFileSync(path.join(out, 'server.pid'), String(server.pid));
while (!/127\.0\.0\.1:\d+/.test(serverOut)) { if (server.exitCode !== null) throw new Error(serverOut); await sleep(100); }
const url = `http://127.0.0.1:${port}/?fnweb_fullscreen=off${extra ? '&' + extra : ''}`;
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
async function shot(c, tag) { try { await c.page.screenshot({ path: path.join(out, `${c.name}-${tag}.png`), timeout: 15000 }); } catch (e) { log(c.name + " screenshot " + tag + " failed"); } }

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
  await shot(host, 'lobby'); await shot(guest, 'lobby');
  log('both in lobby');
  await host.page.waitForFunction(() => window.__fnwebPhase === 'stage:GamePicker', null, { timeout: 900000 }).catch(() => log('host picker wait timed out'));
  await guest.page.waitForFunction(() => window.__fnwebPhase === 'stage:GamePicker', null, { timeout: 900000 }).catch(() => log('guest picker wait timed out'));
  log('both at the picker behind the lobby');
  await host.page.evaluate(() => globalThis.__fnweb_mp_text_send(JSON.stringify({ t: 'start' })));
  for (const c of clients) await c.page.waitForFunction(() => window.__fnwebPhase === 'playing', null, { timeout: 900000 });
  log('both playing');
  await sleep(8000);
  await shot(host, 'spawn'); await shot(guest, 'spawn');
  log('host sees: ' + grep(host, /\[mp\] remote pawn/).join(' | '));
  log('guest sees: ' + grep(guest, /\[mp\] remote pawn/).join(' | '));
  const consoleCmd = async (c, cmd) => { await c.page.keyboard.press('Backquote'); await sleep(400); await c.page.keyboard.type(cmd, { delay: 20 }); await c.page.keyboard.press('Enter'); await sleep(400); await c.page.keyboard.press('Backquote'); await sleep(400); };
  for (const c of [host, guest]) { await c.page.bringToFront(); await c.page.mouse.click(640, 380); await sleep(400); await consoleCmd(c, 'giveresources 500'); await consoleCmd(c, 'giveathenafullkit'); await consoleCmd(c, 'giveathenaammo'); }
  await guest.page.bringToFront();
  await guest.page.mouse.click(640, 380);
  await sleep(500);
  await guest.page.keyboard.down('KeyD'); await sleep(900); await guest.page.keyboard.up('KeyD');
  await sleep(1500);
  await shot(host, 'guest-moved'); await shot(guest, 'guest-moved');
  await guest.page.keyboard.down('KeyA'); await sleep(900); await guest.page.keyboard.up('KeyA');
  await sleep(1000);
  await guest.page.keyboard.press('Digit1');
  await sleep(800);
  await guest.page.keyboard.down('KeyW'); await sleep(250); await guest.page.keyboard.up('KeyW'); await sleep(300);
  for (let i = 0; i < 3; i++) { await guest.page.mouse.down(); await sleep(300); await guest.page.mouse.up(); await sleep(900); }
  await sleep(1500);
  log('melee -> host: ' + grep(host, /\[mp\] took/).slice(0, 8).join(' | ') + ' // guest: ' + grep(guest, /swing stage/).slice(0, 4).join(' | '));
  await shot(host, 'after-melee'); await shot(guest, 'after-melee');
  await guest.page.bringToFront();
  await guest.page.mouse.click(640, 380);
  await guest.page.keyboard.press('KeyQ'); await guest.page.mouse.move(1040, 380, { steps: 12 }); await sleep(300);
  await sleep(2500);
  await guest.page.mouse.down(); await sleep(250); await guest.page.mouse.up(); await sleep(800); await guest.page.mouse.down(); await sleep(250); await guest.page.mouse.up(); await sleep(500); await guest.page.mouse.move(640, 380, { steps: 12 }); await guest.page.keyboard.press('Digit1');
  await sleep(2500);
  await shot(host, 'after-build'); await shot(guest, 'after-build');
  log('host builds: ' + grep(host, /\[mp\] remote build/).join(' | ') + ' // guest sent: ' + grep(guest, /sent build|building world/).join(' | '));
  await host.page.bringToFront();
  await host.page.mouse.click(640, 380);
  await host.page.keyboard.press('KeyQ'); await host.page.mouse.move(1040, 380, { steps: 12 }); await sleep(300);
  await sleep(2500);
  await host.page.mouse.down(); await sleep(250); await host.page.mouse.up(); await sleep(800); await host.page.mouse.down(); await sleep(250); await host.page.mouse.up(); await sleep(500); await host.page.mouse.move(640, 380, { steps: 12 }); await host.page.keyboard.press('Digit1');
  await sleep(2500);
  log('guest builds: ' + grep(guest, /\[mp\] remote build/).join(' | '));
  await shot(host, 'host-build'); await shot(guest, 'host-build');
  await guest.page.bringToFront(); await guest.page.mouse.click(640, 380); await sleep(300);
  await guest.page.keyboard.press('Digit2');
  await sleep(1500);
  await guest.page.mouse.down(); await sleep(1500); await guest.page.mouse.up();
  await sleep(2500);
  await shot(host, 'after-fire'); await shot(guest, 'after-fire');
  log('host damage: ' + grep(host, /\[mp\] took|\[mp\].*eliminated/).slice(0, 8).join(' | '));
  log('guest hits: ' + grep(guest, /\[mp\] hit |hitreg.*fired/).slice(0, 6).join(' | '));
  await host.page.bringToFront();
  await host.page.mouse.click(640, 380);
  await sleep(300);
  await host.page.keyboard.press('Digit2');
  await sleep(1500);
  await host.page.mouse.down(); await sleep(1200); await host.page.mouse.up();
  await sleep(2500);
  log('guest damage: ' + grep(guest, /\[mp\] took|\[mp\].*eliminated/).slice(0, 8).join(' | '));
  log('host hits: ' + grep(host, /\[mp\] hit |hitreg.*fired/).slice(0, 6).join(' | '));
  await shot(host, 'host-fired'); await shot(guest, 'host-fired');
  for (const c of clients) {
    const errs = grep(c, /exception|panicked|The game stopped/);
    if (errs.length) log(c.name + ' errors: ' + errs.slice(0, 5).join(' | '));
  }
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
