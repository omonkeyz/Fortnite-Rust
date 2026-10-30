import { chromium } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { REPO, BRAVE, startServer } from './lib.mjs';

const argv = process.argv.slice(2);
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 ? argv[i + 1] : d; };
const flag = (n) => argv.includes(n);
const worker = opt('--worker', 'https://fortniteog-files.fn-web-worker.workers.dev').replace(/\/+$/, '');
const site = path.resolve(opt('--site', 'D:/fn-site-bench'));
const out = path.resolve(opt('--out', 'D:/fn-verify/webopt/runs/bench'));
const [W, H] = opt('--size', '2544x1353').split('x').map(Number);
const walkSecs = Number(opt('--walk-secs', '60'));
const label = opt('--label', path.basename(out));
const basePage = 'fnweb_fullscreen=off&fnweb_note=off&FN_TOD_HOUR=12&fnweb_quality=hold&fnweb_rung=3';
const pageArgs0 = opt('--page-args', '') ? basePage + '&' + opt('--page-args') : basePage;
const pageArgs = /(^|&)FN_LOBBY=/.test(pageArgs0) ? pageArgs0 : pageArgs0 + (pageArgs0 ? '&' : '') + 'FN_LOBBY=0';
fs.mkdirSync(out, { recursive: true });
const keep = opt('--profile-dir');
const profile = keep ? path.resolve(keep) : fs.mkdtempSync(path.join(os.tmpdir(), 'fnweb-bench-'));
if (keep) fs.mkdirSync(profile, { recursive: true });
const t0 = Date.now();
const logLines = [];
const log = (m) => { const l = `[${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}] ${m}`; console.log(l); logLines.push(l); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const within = (p, ms, what) => Promise.race([p, sleep(ms).then(() => { throw new Error('no answer within ' + ms + ' ms: ' + what); })]);

const rustcRunning = () => { try { return execFileSync('tasklist', ['/FI', 'IMAGENAME eq rustc.exe', '/NH'], { encoding: 'utf8' }).toLowerCase().includes('rustc.exe'); } catch (_) { return false; } };
for (let i = 0; i < 120 && rustcRunning(); i++) { if (i === 0) console.log('waiting for a running cargo build to finish'); await new Promise((r) => setTimeout(r, 10000)); }
let buildSeen = 0;
const buildCulprits = new Set();
const buildTimer = setInterval(() => { if (!rustcRunning()) return; buildSeen++; try { const t = execFileSync('powershell', ['-NoProfile', '-Command', "Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'rustc.exe' -or $_.Name -eq 'cargo.exe' } | ForEach-Object { $_.CommandLine }"], { encoding: 'utf8' }); for (const l of t.split(/\r?\n/)) { const hits = l.match(/target-fn-[\w-]+|--crate-name \S+/g); if (hits) buildCulprits.add(hits.join(' ')); } } catch (_) {} }, 20000);
if (!flag('--no-site')) execFileSync('bash', [path.join(REPO, 'tools/web/site.sh'), '--to', site, '--worker', worker], { stdio: 'inherit' });
if (opt('--mbps') || opt('--rtt')) { const f = path.join(site, 'fnweb-remote.json'); const c = JSON.parse(fs.readFileSync(f, 'utf8')); c.throttle = { mbps: Number(opt('--mbps', '0')), rttMs: Number(opt('--rtt', '0')) }; fs.writeFileSync(f, JSON.stringify(c, null, 1)); }
const siteBuild = (() => { try { return fs.readFileSync(path.join(site, 'build-info.txt'), 'utf8').trim().split(' ').slice(1, 3).join(' '); } catch (_) { return '?'; } })();
console.log('serving ' + site + ' (build ' + siteBuild + ')');
const port = Number(opt('--port', '8781'));
const server = await startServer({ www: site, port, extra: ['--page-args', pageArgs] });
fs.writeFileSync(path.join(out, 'pids.json'), JSON.stringify({ server: server.proc.pid, profile }));
if (!server.url.includes(':' + port)) { server.proc.kill(); throw new Error('port ' + port + ' is taken: ' + server.url); }
const lines = [];
let ctx = null, status = 0, crashed = false;
const result = { label, out, site, siteBuild, pageArgs, size: [W, H], throttle: { mbps: Number(opt('--mbps', '0')), rttMs: Number(opt('--rtt', '0')) }, cold: !keep || !fs.existsSync(path.join(profile, 'Default')), started: new Date().toISOString() };
try {
    ctx = await chromium.launchPersistentContext(profile, {
        executablePath: BRAVE, headless: true, viewport: null,
        args: ['--headless=new', `--window-size=${W},${H}`, '--enable-unsafe-webgpu', '--no-first-run', '--no-default-browser-check', '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows'],
    });
    const page = ctx.pages()[0] || await ctx.newPage();
    page.on('console', (m) => lines.push(`[${((Date.now() - t0) / 1000).toFixed(2)}] [page:${m.type()}] ${m.text()}`));
    page.on('pageerror', (e) => lines.push('[page:exception] ' + (e.stack || e)));
    page.on('crash', () => { crashed = true; lines.push('[page:crash]'); });
    page.on('worker', (w) => w.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') lines.push(`[worker:${m.type()}] ${m.text()}`); }));
    const tNav = Date.now();
    await page.goto(server.url);
    log('headless Brave ' + W + 'x' + H + ' ' + server.url + (result.cold ? ' (cold profile)' : ' (warm profile)'));
    const visible = (sel) => page.evaluate((s) => { const e = document.querySelector(s); return !!e && getComputedStyle(e).display !== 'none' && e.offsetParent !== null; }, sel).catch(() => false);
    const waitVisible = async (sel, secs) => { const end = Date.now() + secs * 1000; while (Date.now() < end) { if (await visible(sel)) return true; if (await visible('#fnweb-login')) throw new Error('the password form appeared (the Worker asked for a password)'); await sleep(200); } throw new Error('not visible: ' + sel); };
    for (let k = 0; k < 120 && !(await page.evaluate(() => !!window.__fnwebReady).catch(() => false)); k++) await new Promise((r) => setTimeout(r, 500));
    result.launchReadySec = (Date.now() - tNav) / 1000;
    await page.mouse.click(W / 2, H / 2);
    log('first click on the game (the user gesture)');
    let phases = [];
    const phaseT = async () => within(page.evaluate(() => (window.__fnwebPhaseLog || []).map((x) => [x.p, x.t])), 5000, 'phases').catch(() => null);
    const waitPhase = async (p, secs) => {
        const end = Date.now() + secs * 1000;
        while (Date.now() < end) {
            if (crashed) throw new Error('page crashed waiting for ' + p);
            const st = await within(page.evaluate(() => [window.__fnwebFatal || null]), 5000, 'eval').catch(() => [null]);
            if (st[0]) throw new Error('FATAL: ' + st[0]);
            const ph = await phaseT(); if (ph) phases = ph;
            if (phases.some((x) => x[0] === p)) return;
            if (await visible('#fnweb-login')) throw new Error('the password form appeared');
            await sleep(250);
        }
        throw new Error('timeout waiting for ' + p);
    };
    await waitPhase('stage:GamePicker', 300);
    result.navToPickerSec = (Date.now() - tNav) / 1000;
    log('game picker after ' + result.navToPickerSec.toFixed(1) + ' s');
    result.swAtPicker = await within(page.evaluate(async () => { try { return await (await fetch('__fnvfs/swstats', { cache: 'no-store' })).json(); } catch (e) { return null; } }), 10000, 'swstats').catch(() => null);
    result.prefetchAtPicker = await page.evaluate(() => window.__fnwebPrefetch || null).catch(() => null);
    log('at the menu: ' + JSON.stringify(result.swAtPicker));
    await sleep(1500);
    for (let k = 0; k < 40; k++) {
        await page.focus('#fnweb-canvas').catch(() => {});
        await within(page.keyboard.press('Enter'), 3000, 'Enter').catch(() => {});
        const ok = await waitPhase('stage:Loading', 4).then(() => true).catch((e) => { if (String(e).includes('timeout')) return false; throw e; });
        if (ok) break;
        if (k === 39) throw new Error('Enter on the picker never started the match');
    }
    log('Enter: loading the match');
    await waitPhase('playing', Number(opt('--frame-secs', '900')));
    result.navToFirstFrameSec = (Date.now() - tNav) / 1000;
    const pt = Object.fromEntries(phases.map(([p, t]) => [p, t]));
    result.timeOrigin = await page.evaluate(() => performance.timeOrigin).catch(() => null);
    result.phases = phases;
    result.enterToFirstFrameSec = (pt['playing'] - pt['stage:Loading']) / 1000;
    result.finishingSec = pt['stage:Finishing'] != null ? (pt['playing'] - pt['stage:Finishing']) / 1000 : null;
    log('first in-game frame: Enter -> first frame ' + result.enterToFirstFrameSec.toFixed(1) + ' s');
    const swstats = () => within(page.evaluate(async () => { try { return await (await fetch('__fnvfs/swstats', { cache: 'no-store' })).json(); } catch (e) { return null; } }), 10000, 'swstats').catch(() => null);
    result.swAtFirstFrame = await swstats();
    result.prefetch = await page.evaluate(() => window.__fnwebPrefetch || null).catch(() => null);
    result.statsAtFirstFrame = await page.evaluate(() => window.__fnwebStats || null).catch(() => null);
    log('worker requests to the first frame: ' + JSON.stringify(result.swAtFirstFrame));
    if (pageArgs.includes('FN_READLOG=1')) { const r = await within(page.evaluate(() => { try { return window.__fnweb.fnweb_read_report(400); } catch (e) { return ''; } }), 20000, 'read report').catch(() => ''); if (r) fs.writeFileSync(path.join(out, 'reads.txt'), r); }
    if (opt('--save-manifest')) { const m = await within(page.evaluate(() => { try { return window.__fnweb.fnweb_fetch_log(); } catch (e) { return ''; } }), 20000, 'fetch log').catch(() => ''); if (m && m.length > 4) { fs.writeFileSync(path.resolve(opt('--save-manifest')), m + String.fromCharCode(10)); log('boot manifest saved: ' + JSON.parse(m).length + ' ranges'); } else log('no fetch log (FNWEB_RECORD=1?)'); }
    await sleep(3000);
    await page.screenshot({ path: path.join(out, '4-world.png'), timeout: 90000 }).catch(() => {});
    const state = async () => { for (let k = 0; ; k++) { try { return await within(page.evaluate(() => Array.from(window.__fnweb.fnweb_bench_state())), 10000, 'state'); } catch (e) { if (k >= 5) throw e; } } };
    await page.mouse.click(W / 2, H / 2);
    await sleep(600);
    let s = await state();
    if (s[4] > 0) { log('menu open after the first frame: Escape'); await page.keyboard.press('Escape'); await sleep(600); s = await state(); }
    result.menuAtStart = s[4];
    const start = { x: s[0], y: s[1], z: s[2], yaw: s[3] };
    log('start ' + JSON.stringify(start));
    const rad = (d) => d * Math.PI / 180;
    const dirOf = (yaw) => [Math.sin(rad(yaw)), -Math.cos(rad(yaw))];
    const h0 = start.yaw + 180;
    const legs = (opt('--legs', '0,50,0,-50,0,50,0,-50')).split(',').map(Number);
    const legLen = Number(opt('--leg-m', '60'));
    const wps = []; { let x = start.x, z = start.z; for (const a of legs) { const [dx, dz] = dirOf(h0 + a); x += dx * legLen; z += dz * legLen; wps.push([x, z]); } }
    result.waypoints = wps;
    let prof = null;
    if (flag('--play-profile')) { prof = await ctx.newCDPSession(page); await prof.send('Profiler.enable'); await prof.send('Profiler.setSamplingInterval', { interval: 500 }); await prof.send('Profiler.start'); }
    await page.evaluate(() => window.__fnweb.fnweb_bench_start());
    await page.keyboard.down('KeyW');
    await page.keyboard.down('ShiftLeft');
    const tw = Date.now();
    let wi = 0, hist = [], detourUntil = 0, detour = 0, jumps = 0, shots = 0, menuSeen = 0;
    while (Date.now() - tw < walkSecs * 1000 && wi < wps.length) {
        s = await state().catch(() => null);
        if (!s) { await sleep(100); continue; }
        if (s[4] > 0) { menuSeen++; await page.keyboard.press('Escape'); await sleep(200); await page.mouse.click(W / 2, H / 2); await sleep(200); continue; }
        const [tx, tz] = wps[wi];
        const dx = tx - s[0], dz = tz - s[2];
        if (Math.hypot(dx, dz) < 5) { wi++; continue; }
        const now = Date.now();
        hist.push([now, s[0], s[2]]); hist = hist.filter((h) => now - h[0] < 2000);
        const moved = hist.length > 1 ? Math.hypot(s[0] - hist[0][1], s[2] - hist[0][2]) : 99;
        if (now - hist[0][0] > 1500 && moved < 2 && now > detourUntil) { detour = (detour >= 0 ? -1 : 1) * (60 + 20 * (jumps % 3)); detourUntil = now + 1800; jumps++; await page.keyboard.press('Space'); }
        let yaw = Math.atan2(dx, -dz) * 180 / Math.PI;
        if (now < detourUntil) yaw += detour;
        await page.evaluate((y) => window.__fnweb.fnweb_bench_yaw(y), yaw);
        if ((now - tw) / 1000 >= shots * 15) { await page.screenshot({ path: path.join(out, `walk-${String(shots * 15).padStart(2, '0')}s.png`), timeout: 20000 }).catch(() => {}); shots++; }
        await sleep(100);
    }
    await page.keyboard.up('ShiftLeft');
    await page.keyboard.up('KeyW');
    const raw = await within(page.evaluate(() => Array.from(window.__fnweb.fnweb_bench_take())), 20000, 'take');
    if (prof) { const { profile: pr } = await within(prof.send('Profiler.stop'), 30000, 'stop'); fs.writeFileSync(path.join(out, 'play.cpuprofile'), JSON.stringify(pr)); log('play profile saved'); }
    await page.screenshot({ path: path.join(out, '5-after-walk.png'), timeout: 30000 }).catch(() => {});
    const N = 10, fr = [];
    for (let i = 0; i + N <= raw.length; i += N) fr.push(raw.slice(i, i + N));
    fs.writeFileSync(path.join(out, 'frames.json'), JSON.stringify(fr));
    const iv = fr.map((f) => f[0]).filter((x) => x > 0 && x < 5000).sort((a, b) => a - b);
    const q = (v, p) => v.length ? v[Math.min(v.length - 1, Math.floor(v.length * p))] : 0;
    const cpu = fr.map((f) => f[1]).sort((a, b) => a - b);
    const gpu = fr.map((f) => f[2]).filter((x) => x > 0).sort((a, b) => a - b);
    let dist = 0; for (let i = 1; i < fr.length; i++) dist += Math.hypot(fr[i][3] - fr[i - 1][3], fr[i][5] - fr[i - 1][5]);
    const secs = iv.reduce((a, b) => a + b, 0) / 1000;
    Object.assign(result, {
        walkSecs: secs, frames: fr.length, fpsMean: fr.length / Math.max(secs, 0.001),
        fpsP50: 1000 / q(iv, 0.5), fpsP1: 1000 / q(iv, 0.99), intervalP50: q(iv, 0.5), intervalP99: q(iv, 0.99), worstMs: iv[iv.length - 1] || 0,
        hitches50: iv.filter((x) => x > 50).length, hitches33: iv.filter((x) => x > 33.4).length,
        cpuP50: q(cpu, 0.5), cpuP95: q(cpu, 0.95), gpuP50: q(gpu, 0.5), gpuP95: q(gpu, 0.95),
        distanceM: dist, waypointsReached: wi, detours: jumps, menuFrames: fr.filter((f) => f[6] > 0).length + menuSeen,
        poiKindsBuilt: fr.reduce((a, f) => a + f[7], 0), hitchPoi: fr.filter((f, i) => i > 0 && f[0] > 50 && fr[i - 1][7] > 0).length, hitchPipe: fr.filter((f, i) => i > 0 && f[0] > 50 && fr[i - 1][8] > 0).length, hitchFile: fr.filter((f, i) => i > 1 && f[0] > 50 && fr[i - 1][9] - fr[i - 2][9] > 20).length,
        mainFileWaitMs: fr.length ? fr[fr.length - 1][9] - fr[0][9] : 0,
    });
    result.buildDuringRun = buildSeen;
    result.buildCulprits = [...buildCulprits].slice(0, 20);
    result.valid = result.menuFrames <= 30 && result.distanceM >= 100 && buildSeen === 0;
    result.swAtEnd = await swstats();
    const st = await page.evaluate(() => window.__fnwebStats || null).catch(() => null);
    result.stats = st;
} catch (e) {
    log('ERROR ' + (e.stack || e)); status = 1; result.error = String(e);
} finally {
    clearInterval(buildTimer);
    fs.writeFileSync(path.join(out, 'console.txt'), lines.join('\n') + '\n');
    fs.writeFileSync(path.join(out, 'launcher.txt'), server.log());
    if (ctx) { try { await within(ctx.close(), 15000, 'close'); } catch (_) {} }
    server.proc.kill();
    await sleep(1000);
    if (!keep) { try { fs.rmSync(profile, { recursive: true, force: true }); } catch (_) {} }
}
const grab = (re) => { const l = lines.find((x) => re.test(x)); return l ? l.replace(/^\[[\d.]+\] \[page:\w+\] /, '') : null; };
result.finishingLine = grab(/\[load\] finishing stage/);
const au = lines.map((l) => /\[fnweb-audio\] underruns=(\d+)/.exec(l)).filter(Boolean).map((m) => Number(m[1]));
result.audioUnderruns = au.length ? Math.max(...au) : null;
result.webtexLine = grab(/\[webtex\] at the first frame/);
result.slowLines = lines.filter((l) => /\[slow\]|sky.*filter|\[guns?\]/i.test(l)).slice(0, 20).map((l) => l.slice(0, 200));
fs.writeFileSync(path.join(out, 'result.json'), JSON.stringify(result, null, 1));
const f1 = (x) => (x == null ? 'n/a' : Number(x).toFixed(1));
const summary = `${label} [${siteBuild}]: ${result.cold ? 'cold' : 'warm'}${result.throttle.mbps ? ' ' + result.throttle.mbps + 'Mbit/' + result.throttle.rttMs + 'ms' : ''} launch ${f1(result.launchReadySec)} s, page->frame ${f1(result.navToFirstFrameSec)} s, enter->frame ${f1(result.enterToFirstFrameSec)} s | fps p50 ${f1(result.fpsP50)} p1 ${f1(result.fpsP1)} mean ${f1(result.fpsMean)} | >50ms ${result.hitches50} (poi ${result.hitchPoi}, pipe ${result.hitchPipe}, file ${result.hitchFile}) worst ${f1(result.worstMs)} ms | main p50 ${f1(result.cpuP50)} p95 ${f1(result.cpuP95)} ms | gpu p50 ${f1(result.gpuP50)} | ${f1(result.distanceM)} m, wp ${result.waypointsReached}, menu ${result.menuFrames}, filewait ${f1(result.mainFileWaitMs)} ms | prep-world ${f1(result.finishingSec)} s | audio underruns ${result.audioUnderruns ?? 'n/a'} | worker req ${result.swAtFirstFrame ? result.swAtFirstFrame.requests : 'n/a'} -> ${result.swAtEnd ? result.swAtEnd.requests : 'n/a'} (${result.swAtEnd ? (result.swAtEnd.bytes / 1048576).toFixed(0) : 'n/a'} MB) | picker ${f1(result.navToPickerSec)} s | ${result.buildDuringRun ? 'cargo build ran during the run | ' : ''}${result.valid ? 'VALID' : 'INVALID'}${result.error ? ' ERROR ' + result.error : ''}`;
log(summary);
fs.appendFileSync(path.join(path.dirname(out), 'summary.txt'), summary + '\n');
process.exit(status);
