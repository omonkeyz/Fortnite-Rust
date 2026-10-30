// Runs one of the game's input scripts in the browser build (headless Brave, temporary profile) with a CPU profile of the page's main thread for
// the whole run, e.g. the first-use freezes: verification/first_use/first_use.script.
// usage: node tools/web/test/script.mjs <file.script> [--out verification/browser/script] [--size 2544x1353] [--env K=V ...] [--secs 300]
//   the script is served from the launcher's /fn/bin folder (copied to dist/web-bin); the run's --out folder is /fn/user/<out name>
//   (= %LOCALAPPDATA%\FortniteRuntime-web\user\<out name>); summarise the profile with tools/web/test/cpuprof.mjs <out>/main-thread.cpuprofile --frames 100
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { REPO, BRAVE, startServer } from './lib.mjs';

const argv = process.argv.slice(2);
const opt = (name, def) => { const i = argv.indexOf(name); return i >= 0 ? argv[i + 1] : def; };
const scriptFile = path.resolve(REPO, argv[0] || 'verification/first_use/first_use.script');
const out = path.resolve(REPO, opt('--out', 'verification/browser/script'));
const [W, H] = opt('--size', '2544x1353').split('x').map(Number);
const secs = Number(opt('--secs', '300'));
const env = []; for (let i = 0; i < argv.length; i++) if (argv[i] === '--env') env.push(argv[i + 1]);
fs.mkdirSync(out, { recursive: true });
const name = path.basename(scriptFile);
const binDir = path.join(REPO, 'dist', 'web-bin');
fs.mkdirSync(binDir, { recursive: true });
fs.copyFileSync(scriptFile, path.join(binDir, name));
const outName = 'script-' + path.basename(out);
const t0 = Date.now();
const log = (m) => console.log(`[${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}] ${m}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const within = (p, ms, what) => Promise.race([p, sleep(ms).then(() => { throw new Error('no answer within ' + ms + ' ms: ' + what); })]);

const profileDir = fs.mkdtempSync(path.join(os.tmpdir(), 'fnweb-script-'));
const server = await startServer({ www: opt('--www', null) ? path.resolve(opt('--www')) : path.join(REPO, 'dist', 'web'), port: 8795 });
const lines = [];
let ctx = null, status = 0, cdp = null;
try {
    ctx = await chromium.launchPersistentContext(profileDir, {
        executablePath: BRAVE, headless: true, viewport: null,
        args: ['--headless=new', '--mute-audio', `--window-size=${W},${H}`, '--enable-unsafe-webgpu', '--no-first-run', '--no-default-browser-check', '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows'],
    });
    const page = ctx.pages()[0] || await ctx.newPage();
    page.on('console', (m) => lines.push(`[${((Date.now() - t0) / 1000).toFixed(2)}] [page:${m.type()}] ${m.text()}`));
    page.on('pageerror', (e) => lines.push('[page:exception] ' + e));
    let crashed = false;
    page.on('crash', () => { crashed = true; lines.push('[page:crash]'); });
    const q = new URLSearchParams({ args: `--script /fn/bin/${name} --out /fn/user/${outName}${opt('--game-args', '') ? ' ' + opt('--game-args', '') : ''}` });
    for (const kv of env) { const i = kv.indexOf('='); q.append(kv.slice(0, i), kv.slice(i + 1)); }
    const url = server.url.replace(/\?.*$/, '') + '?' + q.toString();
    cdp = await ctx.newCDPSession(page);
    await cdp.send('Profiler.enable');
    await cdp.send('Profiler.setSamplingInterval', { interval: 1000 });
    await page.goto(url);
    await cdp.send('Profiler.start');
    log('running ' + name + ' at ' + url);
    const end = Date.now() + secs * 1000;
    let phases = [];
    while (Date.now() < end && !crashed) {
        const st = await within(page.evaluate(() => [window.__fnwebFatal || null, (window.__fnwebPhaseLog || []).map((x) => x.p)]), 10000, 'evaluate').catch(() => [null, phases]);
        if (st[0]) throw new Error('FATAL: ' + st[0]);
        phases = st[1];
        if (phases.includes('exited')) { log('the script ended (the game exited)'); break; }
        await sleep(500);
    }
    if (!phases.includes('exited')) { log('ERROR: the script did not end within ' + secs + ' s'); status = 1; }
    const { profile: prof } = await within(cdp.send('Profiler.stop'), 30000, 'Profiler.stop');
    fs.writeFileSync(path.join(out, 'main-thread.cpuprofile'), JSON.stringify(prof));
    log('CPU profile saved (' + ((prof.endTime - prof.startTime) / 1e6).toFixed(1) + ' s)');
} catch (e) {
    log('ERROR ' + (e.stack || e)); status = 1;
} finally {
    fs.writeFileSync(path.join(out, 'console.txt'), lines.join('\n') + '\n');
    fs.writeFileSync(path.join(out, 'launcher.txt'), server.log());
    if (ctx) { try { await within(ctx.close(), 15000, 'close'); } catch (_) {} }
    server.proc.kill();
    await sleep(1000);
    try { fs.rmSync(profileDir, { recursive: true, force: true }); } catch (_) {}
    // the run's own output folder (frame times etc.), from the launcher's user folder
    const userOut = path.join(process.env.LOCALAPPDATA || '', 'FortniteRuntime-web', 'user', outName);
    if (fs.existsSync(userOut)) { fs.cpSync(userOut, path.join(out, 'run'), { recursive: true }); log('run output copied from ' + userOut); }
}
process.exit(status);
