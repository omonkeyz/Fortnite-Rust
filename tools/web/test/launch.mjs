// The user's path end to end: dist/fortnite-web.exe starts the server AND opens Brave itself (a normal tab; --app: the launcher's app-window
// mode), maximized here, then the normal start-up flow (splash -> start-up loading -> game picker -> Enter = Battle Royale -> loading -> world).
// Brave gets a TEMPORARY profile and a debugging port (only so this script can watch: never the user's own profile), and no WebGPU flag: WebGPU
// must work as shipped.
//
// --headless: the same flow without a window on the user's screens (for measurements while someone uses the machine): the launcher only
// serves (--no-browser), Playwright starts Brave headless (temporary profile, real GPU adapter) with a fixed --size viewport (default 2560x1369 =
// the maximized app window on this machine's main display).
//
// usage: node tools/web/test/launch.mjs [--out verification/browser/launch] [--secs 30] [--page-args K=V&..] [--cpu-profile] [--app]
//                                       [--headless [--size WxH]]
//        (hold the GPU lock: tools/web/test/locked.sh)
import { chromium } from 'playwright-core';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { REPO, BRAVE, startServer } from './lib.mjs';

const argv = process.argv.slice(2);
const opt = (name, def) => { const i = argv.indexOf(name); return i >= 0 ? argv[i + 1] : def; };
const out = path.resolve(REPO, opt('--out', 'verification/browser/launch'));
const playSecs = Number(opt('--secs', '30'));
const pageArgs0 = opt('--page-args', '');
const pageArgs = /(^|&)FN_LOBBY=/.test(pageArgs0) ? pageArgs0 : pageArgs0 + (pageArgs0 ? '&' : '') + 'FN_LOBBY=0';
// --cpu-profile: a V8 CPU profile of the page's main thread from Enter on the picker to 2 s after the first in-game frame
// (out/main-thread.cpuprofile + out/phases.json; summarise with tools/web/test/cpuprof.mjs, or open the profile in DevTools > Performance)
const cpuProfile = argv.includes('--cpu-profile');
// --trace: a browser trace (GPU process included: Dawn pipeline / shader work) from Enter to the first in-game frame -> out/trace.json
// (summarise with tools/web/test/tracestat.mjs)
const trace = argv.includes('--trace');
const headless = argv.includes('--headless');
const appWindow = argv.includes('--app');
const [W, H] = opt('--size', '2560x1369').split('x').map(Number);
fs.mkdirSync(out, { recursive: true });
// --profile-dir <dir>: reuse this browser profile (kept afterwards), e.g. to measure a second launch with the browser's shader cache warm
// --remote (with --headless): REMOTE MODE end to end on this machine: a second launcher plays the private file Worker (--cors / --test-password:
// the same API, the user's real files), the page comes from a tools/web/site.sh folder on port 8780, logs in, reads everything through its
// service worker; with --profile-dir a second run measures the warm service-worker cache
const remoteMode = argv.includes('--remote');
// --latency-ms N / --mbps N (with --remote): the stand-in Worker answers like one across the internet (N ms per request, the bytes at N Mbit/s)
const latencyMs = opt('--latency-ms', '0'), mbps = opt('--mbps', '0');
// --page-port N (with --remote): the page's port (default 8780; another one while a real-Worker test session holds 8780)
const pagePort = Number(opt('--page-port', '8780'));
// --twice: after the first in-game frame, reload the page in the same tab and load the match again (the second launch: a warm service-worker
// cache in remote mode, the browser's warm caches otherwise); prints both Enter -> first frame times
const twice = argv.includes('--twice');
const keepProfile = opt('--profile-dir');
const profile = keepProfile ? path.resolve(keepProfile) : fs.mkdtempSync(path.join(os.tmpdir(), 'fnweb-launch-'));
if (keepProfile) fs.mkdirSync(profile, { recursive: true });
const port = 8794, dbg = 9334;
const t0 = Date.now();
const log = (m) => console.log(`[${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}] ${m}`);

let server = null, serverOut = '', browser = null, ctx = null, status = 0, apiServer = null;
const lines = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
/// `p`, or a rejection after `ms` (a hung or crashed page must not hang this script with the GPU lock held)
const within = (p, ms, what) => Promise.race([p, sleep(ms).then(() => { throw new Error('no answer within ' + ms + ' ms: ' + what); })]);
let crashed = false;
let cdp = null;
/// the CPU profile so far (main thread), e.g. of a hang: Profiler.stop is answered even while the page's script runs
async function dumpProfile(name) {
    if (!cdp) return;
    const c = cdp; cdp = null;
    try {
        const { profile: prof } = await within(c.send('Profiler.stop'), 20000, 'Profiler.stop');
        fs.writeFileSync(path.join(out, name), JSON.stringify(prof));
        log('CPU profile saved: ' + name + ' (' + ((prof.endTime - prof.startTime) / 1e6).toFixed(1) + ' s)');
    } catch (e) { log('no CPU profile: ' + e.message); }
}
try {
    let page = null;
    let remotePassword = null;
    if (remoteMode) {
        remotePassword = 'test-' + Math.random().toString(36).slice(2);
        const api = await startServer({ www: path.join(REPO, 'dist', 'web'), port: 8788, extra: ['--cors', 'http://127.0.0.1:' + pagePort, '--test-password', remotePassword, '--test-latency-ms', latencyMs, '--test-mbps', mbps, ...(opt('--stand-in-assets') ? ['--assets', opt('--stand-in-assets')] : [])] });
        apiServer = api.proc;
        const { execFileSync } = await import('node:child_process');
        execFileSync('bash', [path.join(REPO, 'tools/web/site.sh'), '--to', path.join(REPO, 'dist', 'web-remote-e2e'), '--worker', 'http://127.0.0.1:8788'], { stdio: 'inherit' });
        // --site-overlay <dir>: these page files replace the site's (e.g. an older index.html / sw.js for a before/after comparison); files the
        // overlay lacks that the new page added (the prefetch worker + its manifest) are removed
        const overlay = opt('--site-overlay');
        if (overlay) {
            const site = path.join(REPO, 'dist', 'web-remote-e2e');
            for (const f of ['fnweb-prefetch.js', 'fnweb-boot-manifest.json']) if (!fs.existsSync(path.join(overlay, f))) fs.rmSync(path.join(site, f), { force: true });
            for (const f of ['index.html', 'sw.js', 'fnweb-boot.js', 'worker.js']) if (fs.existsSync(path.join(overlay, f))) fs.copyFileSync(path.join(overlay, f), path.join(site, f));
            log('site overlay: ' + overlay);
        }
    }
    if (headless) {
        const s = await startServer({ www: path.join(REPO, 'dist', remoteMode ? 'web-remote-e2e' : 'web'), port: remoteMode ? pagePort : port, extra: pageArgs ? ['--page-args', pageArgs] : [] });
        server = s.proc; server.stdout.on('data', (d) => { serverOut += d; }); server.stderr.on('data', (d) => { serverOut += d; });
        ctx = await chromium.launchPersistentContext(profile, {
            executablePath: BRAVE, headless: true, viewport: null,
            args: ['--headless=new', `--window-size=${W},${H}`, '--enable-unsafe-webgpu', '--no-first-run', '--no-default-browser-check', '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows'],
        });
        page = ctx.pages()[0] || await ctx.newPage();
        page.on('console', (m) => lines.push(`[page:${m.type()}] ${m.text()}`));
        page.on('pageerror', (e) => lines.push('[page:exception] ' + (e.stack || e)));
        page.on('crash', () => { crashed = true; lines.push('[page:crash] the page crashed'); });
        page.on('worker', (w) => w.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') lines.push(`[worker:${m.type()}] ${m.text()}`); }));
        await page.goto(s.url);
        log('headless Brave ' + W + 'x' + H + ' on ' + s.url);
    } else {
        server = spawn(path.join(REPO, 'dist', 'fortnite-web.exe'), ['--port', String(port), '--browser', BRAVE, ...(pageArgs ? ['--page-args', pageArgs] : []), ...(appWindow ? ['--app'] : []), '--browser-args', `--user-data-dir=${profile} --remote-debugging-port=${dbg} --no-first-run --no-default-browser-check${appWindow ? '' : ' --start-maximized'}`], { stdio: ['ignore', 'pipe', 'pipe'] });
        server.stdout.on('data', (d) => { serverOut += d; });
        server.stderr.on('data', (d) => { serverOut += d; });
        // attach to the Brave the launcher started
        for (let i = 0; i < 100 && !browser; i++) { try { browser = await chromium.connectOverCDP(`http://127.0.0.1:${dbg}`); } catch (_) { await new Promise((r) => setTimeout(r, 300)); } }
        if (!browser) throw new Error('Brave did not come up: ' + serverOut);
        log('attached to the Brave window the launcher opened');
        for (let i = 0; i < 100 && !page; i++) { page = browser.contexts().flatMap((c) => c.pages()).find((p) => p.url().includes(`:${port}`)) || null; if (!page) await new Promise((r) => setTimeout(r, 200)); }
        if (!page) throw new Error('no game page');
        page.on('console', (m) => lines.push(`[page:${m.type()}] ${m.text()}`));
        page.on('pageerror', (e) => lines.push('[page:exception] ' + (e.stack || e)));
        page.on('crash', () => { crashed = true; lines.push('[page:crash] the page crashed'); });
    }
    const has = (s) => lines.some((l) => l.includes(s));
    // the page's own phase log too (index.html): it holds the stages even when this script attached late
    let phases = [];
    const phase = (p) => phases.includes(p);
    const wait = async (what, pred, secs) => {
        const end = Date.now() + secs * 1000;
        while (Date.now() < end) {
            if (pred()) { log(what); return true; }
            if (crashed) throw new Error('the page crashed while waiting for: ' + what);
            const st = await within(page.evaluate(() => [window.__fnwebFatal || null, (window.__fnwebPhaseLog || []).map((x) => x.p)]), 5000, 'page.evaluate').catch(() => [null, phases]);
            if (st[0]) throw new Error('FATAL: ' + st[0]);
            phases = st[1];
            await new Promise((r) => setTimeout(r, 250));
        }
        throw new Error('timeout: ' + what);
    };
    if (remoteMode) {
        // the login form, unless the tab still holds a token (a restored session of a kept profile)
        const form = await page.waitForFunction(() => (getComputedStyle(document.getElementById('fnweb-login')).display !== 'none' ? 'form' : null) || ((window.__fnwebPhaseLog || []).length ? 'running' : null), null, { timeout: 30000 }).then((h) => h.jsonValue());
        if (form === 'form') {
            await page.fill('#fnweb-password', remotePassword);
            await page.click('#fnweb-login-go');
            log('remote mode: logged in through the page');
        } else log('remote mode: no login asked (the tab kept its token)');
    }
    // --boot-profile: the main thread's CPU profile from here on (saved as hang.cpuprofile when a wait times out)
    if (argv.includes('--boot-profile')) {
        cdp = await page.context().newCDPSession(page);
        await cdp.send('Profiler.enable');
        await cdp.send('Profiler.setSamplingInterval', { interval: 2000 });
        await cdp.send('Profiler.start');
        log('CPU profiler started at boot (main thread)');
    }
    log('page ' + page.url() + ' viewport ' + JSON.stringify(await page.evaluate(() => [innerWidth, innerHeight, devicePixelRatio, crossOriginIsolated, 'gpu' in navigator])));
    await wait('game running (splash)', () => has('[web] fortnite-web') || has('-> StartupLoading') || phase('stage:StartupLoading'), 120);
    await new Promise((r) => setTimeout(r, 700));
    await page.screenshot({ path: path.join(out, '1-splash.png'), timeout: 90000 }).catch((e) => log('screenshot skipped: ' + String(e).split(String.fromCharCode(10))[0]));
    await wait('game picker', () => has('-> GamePicker') || phase('stage:GamePicker'), Number(opt('--picker-secs', '120')));
    if (argv.includes('--boot-profile')) await dumpProfile('boot.cpuprofile');
    if (phase('stage:Loading')) throw new Error('the picker was passed before this script could press Enter (someone used the window?)');
    // --lobby-secs N: stay in the lobby (game picker) N s before pressing Play, as a player does (the match prepares in the background meanwhile)
    const lobbySecs = Number(opt('--lobby-secs', '1.5'));
    await new Promise((r) => setTimeout(r, lobbySecs * 1000));
    log('lobby: ' + lobbySecs + ' s in the game picker before Play');
    await page.screenshot({ path: path.join(out, '2-picker.png'), timeout: 90000 }).catch((e) => log('screenshot skipped: ' + String(e).split(String.fromCharCode(10))[0]));
    if (argv.includes('--heap-report')) { const r = await within(page.evaluate(() => { try { return window.__fnweb.fnweb_heap_report(60); } catch (e) { return 'n/a: ' + e; } }), 20000, 'heap report').catch(() => null); if (r) fs.writeFileSync(path.join(out, 'heap-picker.txt'), r); }
    if (cpuProfile) {
        cdp = await page.context().newCDPSession(page);
        await cdp.send('Profiler.enable');
        await cdp.send('Profiler.setSamplingInterval', { interval: 1000 });
        await cdp.send('Profiler.start');
        log('CPU profiler started (main thread)');
    }
    let tracer = null;
    if (trace) {
        tracer = await page.context().newCDPSession(page);
        await tracer.send('Tracing.start', { transferMode: 'ReturnAsStream', traceConfig: { recordMode: 'recordAsMuchAsPossible', includedCategories: (opt('--trace-categories', 'gpu.dawn,blink.user_timing')).split(',') } });
        log('browser trace started');
    }
    // the key's acknowledgement waits for the page's main thread, which the first loading step holds for seconds: do not wait for it
    await within(page.keyboard.press('Enter'), 3000, 'Enter').catch(() => {});
    log('Enter on the picker (Battle Royale)');
    await wait('loading the match', () => has('-> Loading') || phase('stage:Loading'), 30);
    await new Promise((r) => setTimeout(r, 3000));
    await page.screenshot({ path: path.join(out, '3-loading.png'), timeout: 90000 }).catch((e) => log('screenshot skipped: ' + String(e).split(String.fromCharCode(10))[0]));
    // --heap-report (with --page-args fnweb_heaptrack=1): the allocation sites holding the heap, every 10 s of the match load -> out/heap-<s>.txt
    let heapTimer = null;
    if (argv.includes('--heap-report')) {
        const tl = Date.now();
        heapTimer = setInterval(async () => {
            const r = await within(page.evaluate(() => { try { return window.__fnweb.fnweb_heap_report(40); } catch (e) { return 'n/a: ' + e; } }), 8000, 'heap report').catch(() => null);
            if (r) fs.writeFileSync(path.join(out, 'heap-' + Math.round((Date.now() - tl) / 1000) + '.txt'), r);
        }, 10000);
    }
    try { await wait('first in-game frame', () => has('[web] first in-game frame presented') || phase('playing'), Number(opt('--frame-secs', '150'))); }
    finally { if (heapTimer) clearInterval(heapTimer); }
    await new Promise((r) => setTimeout(r, 2000));
    if (cdp) {
        const now = await page.evaluate(() => performance.now());
        const { profile: prof } = await cdp.send('Profiler.stop');
        cdp = null;
        // the page's phase log (performance.now() ms) mapped onto the profile clock (the profile ends ~ at the stop request)
        const ph = await page.evaluate(() => window.__fnwebPhaseLog || []);
        const endMs = (prof.endTime - prof.startTime) / 1000;
        const marks = ph.map((p) => ({ phase: p.p, profileMs: Math.round(endMs - (now - p.t)) }));
        fs.writeFileSync(path.join(out, 'main-thread.cpuprofile'), JSON.stringify(prof));
        fs.writeFileSync(path.join(out, 'phases.json'), JSON.stringify(marks, null, 1));
        log('CPU profile saved (' + (endMs / 1000).toFixed(1) + ' s); phases at profile ms: ' + marks.map((m) => m.phase + '@' + m.profileMs).join(' '));
    }
    if (tracer) {
        const done = new Promise((r) => tracer.once('Tracing.tracingComplete', r));
        await tracer.send('Tracing.end');
        const { stream } = await within(done, 60000, 'tracingComplete');
        const chunks = [];
        for (;;) { const r = await tracer.send('IO.read', { handle: stream, size: 8 << 20 }); chunks.push(r.base64Encoded ? Buffer.from(r.data, 'base64') : Buffer.from(r.data)); if (r.eof) break; }
        await tracer.send('IO.close', { handle: stream });
        fs.writeFileSync(path.join(out, 'trace.json'), Buffer.concat(chunks));
        log('browser trace saved (' + (Buffer.concat(chunks).length >> 20) + ' MB)');
        tracer = null;
    }
    await page.screenshot({ path: path.join(out, '4-world.png'), timeout: 90000 }).catch((e) => log('screenshot skipped: ' + String(e).split(String.fromCharCode(10))[0]));
    // FN_READLOG=1 in --page-args: what the load read from the paks (fn-pak read log)
    const reads = await within(page.evaluate(() => { try { return window.__fnweb.fnweb_read_report(60); } catch (e) { return ''; } }), 10000, 'read report').catch(() => '');
    if (reads && !reads.includes('empty')) { fs.writeFileSync(path.join(out, 'reads.txt'), reads); log('pak read log saved: ' + reads.split(String.fromCharCode(10))[0]); }
    // --save-manifest <file> (with --page-args FNWEB_RECORD=1): the load's file requests -> the remote-mode start-up prefetch list
    const manifestOut = opt('--save-manifest');
    if (manifestOut) {
        const m = await within(page.evaluate(() => { try { return window.__fnweb.fnweb_fetch_log(); } catch (e) { return ''; } }), 10000, 'fetch log').catch(() => '');
        if (m && m.length > 4) { fs.writeFileSync(path.resolve(REPO, manifestOut), m + String.fromCharCode(10)); log('boot manifest saved: ' + JSON.parse(m).length + ' ranges -> ' + manifestOut); } else log('no fetch log (FNWEB_RECORD=1 missing?)');
    }
    log('load: Enter -> first frame ' + await page.evaluate(() => { const l = window.__fnwebPhaseLog || []; const a = l.find((x) => x.p === 'stage:Loading'), b = l.find((x) => x.p === 'playing'); return a && b ? ((b.t - a.t) / 1000).toFixed(1) + ' s' : 'n/a'; }) + '; prefetch ' + JSON.stringify(await page.evaluate(() => window.__fnwebPrefetch || null)) + '; stats ' + JSON.stringify(await page.evaluate(() => window.__fnwebStats || null)));
    if (twice) {
        const firstRun = (await page.evaluate(() => { const l = window.__fnwebPhaseLog || []; const a = l.find((x) => x.p === 'stage:Loading'), b = l.find((x) => x.p === 'playing'); return a && b ? b.t - a.t : null; }));
        log('first launch: Enter -> first frame ' + (firstRun / 1000).toFixed(1) + ' s; reloading for the second launch');
        lines.push('[harness] ---- reload: second launch ----');
        await page.reload();
        // the phase log is per page: poll it directly
        for (let i = 0; i < 480; i++) { const ph = await within(page.evaluate(() => (window.__fnwebPhaseLog || []).map((x) => x.p)), 5000, 'eval').catch(() => []); if (ph.includes('stage:GamePicker')) break; await sleep(250); }
        await sleep(1500);
        await within(page.keyboard.press('Enter'), 3000, 'Enter').catch(() => {});
        for (let i = 0; i < 960; i++) { const ph = await within(page.evaluate(() => (window.__fnwebPhaseLog || []).map((x) => x.p)), 5000, 'eval').catch(() => []); if (ph.includes('playing')) break; await sleep(250); }
        const secondRun = await page.evaluate(() => { const l = window.__fnwebPhaseLog || []; const a = l.find((x) => x.p === 'stage:Loading'), b = l.find((x) => x.p === 'playing'); return a && b ? b.t - a.t : null; });
        log('second launch: Enter -> first frame ' + (secondRun == null ? 'n/a' : (secondRun / 1000).toFixed(1) + ' s'));
        await sleep(2000);
        await page.screenshot({ path: path.join(out, '4b-world-second-launch.png'), timeout: 90000 }).catch((e) => log('screenshot skipped: ' + String(e).split(String.fromCharCode(10))[0]));
    }
    // click = pointer lock (the game's capture), then walk + look for a while
    const vp = await page.evaluate(() => [innerWidth, innerHeight]);
    await page.mouse.click(vp[0] / 2, vp[1] / 2);
    await new Promise((r) => setTimeout(r, 500));
    log('pointer locked: ' + await page.evaluate(() => !!document.pointerLockElement));
    await page.keyboard.down('KeyW');
    const tw = Date.now();
    let i = 0;
    // --play-profile: a CPU profile of the page's main thread over 10 s of the walk, starting 20 s in (steady play) -> out/play.cpuprofile
    let playProf = null, playProfStarted = false, playProfDone = false;
    const profFromMs = Number(opt('--play-profile-from', '20')) * 1000, profLenMs = Number(opt('--play-profile-secs', '10')) * 1000;
    const walkStats = [];
    let lastStat = 0;
    while (Date.now() - tw < playSecs * 1000) {
        await page.mouse.move(vp[0] / 2 + Math.round(Math.sin(i / 20) * 20), vp[1] / 2); await new Promise((r) => setTimeout(r, 50)); i++;
        if (argv.includes('--play-profile') && !playProfStarted && Date.now() - tw >= profFromMs) { playProfStarted = true; playProf = await page.context().newCDPSession(page); await playProf.send('Profiler.enable'); await playProf.send('Profiler.setSamplingInterval', { interval: 500 }); await playProf.send('Profiler.start'); log('play profile started'); }
        if (playProf && !playProfDone && Date.now() - tw > profFromMs + profLenMs) { playProfDone = true; const { profile: pr } = await within(playProf.send('Profiler.stop'), 20000, 'stop').catch(() => ({})); if (pr) { fs.writeFileSync(path.join(out, 'play.cpuprofile'), JSON.stringify(pr)); log('play profile saved (' + profLenMs / 1000 + ' s)'); } }
        if (Date.now() - lastStat > 5000) { lastStat = Date.now(); const st = await within(page.evaluate(() => window.__fnwebStats || null), 5000, 'stats').catch(() => null); if (st) walkStats.push(st); }
    }
    if (walkStats.length) {
        const g = walkStats.map((x) => x.gameFps);
        log('walk: game fps per 5 s window ' + g.map((x) => x.toFixed(0)).join(' ') + ' (mean ' + (g.reduce((a, b) => a + b, 0) / g.length).toFixed(1) + '); main thread file wait ' + walkStats[0].mainFileWaitSec.toFixed(1) + ' -> ' + walkStats[walkStats.length - 1].mainFileWaitSec.toFixed(1) + ' s; http ' + walkStats[0].http + ' -> ' + walkStats[walkStats.length - 1].http);
    }
    await page.keyboard.up('KeyW');
    await page.screenshot({ path: path.join(out, '5-after-walk.png'), timeout: 90000 }).catch((e) => log('screenshot skipped: ' + String(e).split(String.fromCharCode(10))[0]));
    // --console-test: Tilde (Backquote) opens the dev console (fn-console); type commands, Enter runs each; Tilde closes it. The game logs every reply as "[console] ...".
    if (argv.includes('--console-test')) {
        const before = lines.length;
        await page.keyboard.press('Backquote'); await sleep(400);
        log('console open: pointer locked ' + await page.evaluate(() => !!document.pointerLockElement));
        for (const cmd of ['fly', 'speed 2', 'storm']) { await page.keyboard.type(cmd, { delay: 20 }); await page.keyboard.press('Enter'); await sleep(500); }
        await page.screenshot({ path: path.join(out, '5b-console.png'), timeout: 90000 }).catch((e) => log('screenshot skipped: ' + String(e).split(String.fromCharCode(10))[0]));
        await page.keyboard.press('Backquote'); await sleep(300);
        const replies = lines.slice(before).filter((l) => l.includes('[console]'));
        log('console replies (' + replies.length + '): ' + replies.slice(0, 8).map((l) => l.replace(/^[page:log] /, '')).join(' | '));
    }
    const hitchLine = lines.find((l) => l.includes('[hitch] first 60 s'));
    log('hitches: ' + (hitchLine ? hitchLine.replace(/^[page:log] /, '') : 'no summary (walk shorter than 60 s?)') + '; ' + lines.filter((l) => l.includes('[hitch] t=')).length + ' frames over 20 ms logged');
    const st = await page.evaluate(() => window.__fnwebStats || null);
    log('stats ' + JSON.stringify(st));
    log('canvas [backing w, h, css w, h, dpr]: ' + JSON.stringify(await page.evaluate(() => { const c = document.getElementById('fnweb-canvas'); return [c.width, c.height, c.clientWidth, c.clientHeight, devicePixelRatio]; })));
    // Esc releases the mouse and opens the menu (as in the exe)
    await page.keyboard.press('Escape');
    await new Promise((r) => setTimeout(r, 800));
    log('after Esc: pointer locked ' + await page.evaluate(() => !!document.pointerLockElement));
    await page.screenshot({ path: path.join(out, '6-menu.png'), timeout: 90000 }).catch((e) => log('screenshot skipped: ' + String(e).split(String.fromCharCode(10))[0]));
} catch (e) {
    log('ERROR ' + (e.stack || e)); status = 1;
    await dumpProfile('hang.cpuprofile');
} finally {
    fs.writeFileSync(path.join(out, 'console.txt'), lines.join('\n') + '\n');
    fs.writeFileSync(path.join(out, 'launcher.txt'), serverOut);
    if (ctx) { try { await within(ctx.close(), 15000, 'close'); } catch (_) {} }
    try { const c = browser && browser.contexts()[0]; if (c) { const s = await within(c.newCDPSession(c.pages()[0]), 5000, 'cdp'); await within(s.send('Browser.close'), 5000, 'Browser.close').catch(() => {}); } } catch (_) {}
    try { await within(browser?.close() ?? Promise.resolve(), 10000, 'browser.close'); } catch (_) {}
    server?.kill();
    apiServer?.kill();
    await new Promise((r) => setTimeout(r, 1500));
    if (!keepProfile) { try { fs.rmSync(profile, { recursive: true, force: true }); } catch (_) {} }
}
for (const l of lines.filter((l) => /\[startup|auto-quality|first in-game|error|panicked|\[perf\]|\[spawn\]|finishing stage|post-load setup|\[hitch\] first/i.test(l)).slice(0, 60)) console.log('  ' + l.slice(0, 300));
process.exit(status);
