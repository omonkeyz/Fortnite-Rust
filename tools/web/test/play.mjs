// Drives the browser build in Brave (temporary profile): starts dist/fortnite-web.exe, opens the page with game arguments, waits for the
// world, optionally walks around with real key / mouse input, takes screenshots, samples the frame rate and the wasm heap, saves the console.
//
// --cloud <worker url>: REMOTE MODE against the real Cloudflare file Worker: the page comes from a tools/web/site.sh folder on 127.0.0.1:8780 (an
//   allowed origin of the Worker) and every game file is read from the bucket; the PASSWORD IS TYPED BY THE USER in the opened window (the harness
//   waits up to --login-secs, default 600, for the game to start).
// usage: node tools/web/test/play.mjs [--args "--skip-loading"] [--size 2560x1400] [--secs 60] [--walk] [--out verification/browser/run1] [--query "a=b"]
// Hold the GPU lock around it: verification/perf_assets/tools/gpu_lock.sh acquire "browser-port: ..." / release
import { startServer, launchBrowser, REPO } from './lib.mjs';
import fs from 'node:fs';
import path from 'node:path';

const argv = process.argv.slice(2);
const opt = (name, def) => { const i = argv.indexOf(name); return i >= 0 ? argv[i + 1] : def; };
const flag = (name) => argv.includes(name);
const gameArgs = opt('--args', '--skip-loading');
const [W, H] = opt('--size', '1600x900').split('x').map(Number);
const secs = Number(opt('--secs', '90'));
const out = path.resolve(REPO, opt('--out', 'verification/browser/play'));
const query = opt('--query', '');
const waitFor = opt('--wait-for', '[load] done');
fs.mkdirSync(out, { recursive: true });

const cloud = opt('--cloud', '');
let server;
if (cloud) {
    const { execFileSync } = await import('node:child_process');
    execFileSync('bash', [path.join(REPO, 'tools/web/site.sh'), '--to', path.join(REPO, 'dist', 'web-cloud'), '--worker', cloud], { stdio: 'inherit' });
    server = await startServer({ www: path.join(REPO, 'dist', 'web-cloud'), port: 8780 });
} else server = await startServer({ www: path.join(REPO, 'dist', 'web') });
const b = await launchBrowser({ width: W, height: H });
const t0 = Date.now();
const log = (m) => { const l = `[${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}] ${m}`; console.log(l); };
const save = () => fs.writeFileSync(path.join(out, 'console.txt'), b.lines.join('\n') + '\n');
let status = 0;
try {
    const url = server.url + '?' + new URLSearchParams({ args: gameArgs }).toString() + (query ? '&' + query : '') + (/(^|&)FN_LOBBY=/.test(query || '') ? '' : '&FN_LOBBY=0');
    // --profile: sample the page's main thread (JS + wasm functions) from navigation until the world has been drawn for a few seconds
    let cdp = null;
    if (flag('--profile')) {
        cdp = await b.ctx.newCDPSession(b.page);
        await cdp.send('Profiler.enable');
        await cdp.send('Profiler.setSamplingInterval', { interval: 2000 });
        await cdp.send('Profiler.start');
        log('main-thread CPU profiler started');
    }
    globalThis.__fnwebStopProfile = async () => {
        if (!cdp) return;
        const { profile } = await cdp.send('Profiler.stop');
        fs.writeFileSync(path.join(out, 'main-thread.cpuprofile'), JSON.stringify(profile));
        // self time per function (top 40)
        const byId = new Map(profile.nodes.map((n) => [n.id, n]));
        const self = new Map();
        const dt = profile.timeDeltas;
        for (let i = 0; i < profile.samples.length; i++) {
            const n = byId.get(profile.samples[i]);
            const k = n.callFrame.functionName || ('(' + n.callFrame.url.split('/').pop() + ')');
            self.set(k, (self.get(k) || 0) + (dt[i] || 0) / 1000);
        }
        const top = [...self.entries()].sort((a, b) => b[1] - a[1]).slice(0, 40);
        const lines = top.map(([k, ms]) => `${(ms / 1000).toFixed(2).padStart(8)} s  ${k.slice(0, 160)}`);
        fs.writeFileSync(path.join(out, 'main-thread-top.txt'), lines.join('\n') + '\n');
        log('main-thread profile: top self time\n' + lines.slice(0, 25).join('\n'));
        cdp = null;
    };
    if (flag('--maximize')) {
        // the user's way of playing: a maximized browser window
        const c = await b.ctx.newCDPSession(b.page);
        const { windowId } = await c.send('Browser.getWindowForTarget');
        await c.send('Browser.setWindowBounds', { windowId, bounds: { windowState: 'maximized' } });
        await new Promise((r) => setTimeout(r, 500));
        log('window maximized: viewport ' + JSON.stringify(await b.page.evaluate(() => [innerWidth, innerHeight, devicePixelRatio])));
    }
    log('open ' + url);
    await b.page.goto(url);
    // wait for the world (or a fatal error); in cloud mode the user logs in first
    if (cloud) log('CLOUD: waiting for the password to be typed in the window (' + cloud + ')');
    const deadline = Date.now() + (cloud ? Number(opt('--login-secs', '600')) * 1000 : 0) + secs * 1000;
    let loaded = false;
    let shots = 0;
    let entered = false;
    while (Date.now() < deadline) {
        await new Promise((r) => setTimeout(r, 1000));
        // the normal start-up flow (no --skip-loading): Enter on the game picker = Battle Royale, as the user does
        if (!entered && b.lines.some((l) => l.includes('-> GamePicker') || l.includes('stage:GamePicker'))) { await b.page.keyboard.press('Enter').catch(() => {}); entered = true; log('Enter on the game picker'); }
        const fatal = await b.page.evaluate(() => window.__fnwebFatal || null);
        if (fatal) { log('FATAL: ' + fatal.split('\n')[0]); status = 1; break; }
        if (!loaded && b.lines.some((l) => l.includes(waitFor))) { loaded = true; log('world loaded (' + waitFor + ')'); break; }
        if ((Date.now() - t0) % 15000 < 1000) { await b.page.screenshot({ path: path.join(out, `loading-${shots++}.png`) }); }
    }
    if (loaded) {
        // the world is drawn once frames flow again (first-frame lazy loads can hold the main thread for a while)
        const fpsDeadline = Date.now() + secs * 1000;
        while (Date.now() < fpsDeadline) {
            const st = await b.page.evaluate(() => window.__fnwebStats || null);
            if (st && st.fps > 10) break;
            await new Promise((r) => setTimeout(r, 1000));
        }
        log('frames flowing: ' + JSON.stringify(await b.page.evaluate(() => window.__fnwebStats || null)));
        await new Promise((r) => setTimeout(r, 3000));
        if (!flag('--profile-play')) await globalThis.__fnwebStopProfile(); // --profile-play: keep sampling through the walk (first-seconds hitches)
        await b.page.screenshot({ path: path.join(out, 'world.png') });
        if (flag('--walk')) {
            // focus + pointer lock need a real click; then walk forward (W) while turning with the mouse, sprinting in bursts, a jump now and then
            const walkSecs = Number(opt('--walk-secs', '6'));
            await b.page.mouse.click(W / 2, H / 2);
            await new Promise((r) => setTimeout(r, 500));
            const locked = await b.page.evaluate(() => !!document.pointerLockElement);
            log('pointer locked after click: ' + locked);
            await b.page.keyboard.down('KeyW');
            const tw = Date.now();
            let i = 0, nextShot = 0, nextSample = 0;
            while (Date.now() - tw < walkSecs * 1000) {
                const dx = Math.round(Math.sin(i / 25) * 25);
                await b.page.mouse.move(W / 2 + dx, H / 2 + (i % 2));
                if (i % 120 === 60) { await b.page.keyboard.press('Space'); }
                if (i % 200 === 100) { await b.page.keyboard.down('ShiftLeft'); } else if (i % 200 === 160) { await b.page.keyboard.up('ShiftLeft'); }
                const t = (Date.now() - tw) / 1000;
                if (t >= nextShot) { await b.page.screenshot({ path: path.join(out, `walk-${String(Math.round(t)).padStart(3, '0')}s.png`) }); nextShot += Math.max(10, walkSecs / 6); }
                if (t >= nextSample) { const st = await b.page.evaluate(() => window.__fnwebStats || null); if (st) log(`walk ${t.toFixed(0)} s: ${st.fps.toFixed(1)} fps, heap ${st.heapMB.toFixed(0)} MB (peak ${st.peakMB.toFixed(0)}), wasm ${st.wasmMB.toFixed(0)} MB`); nextSample += 10; }
                await new Promise((r) => setTimeout(r, 50));
                i++;
            }
            await b.page.keyboard.up('ShiftLeft');
            await b.page.keyboard.up('KeyW');
            await b.page.screenshot({ path: path.join(out, 'walk.png') });
        }
        await globalThis.__fnwebStopProfile();
        // frame rate over 10 s (requestAnimationFrame callbacks of the page = presented frames)
        const fps = await b.page.evaluate(() => new Promise((res) => { let n = 0; const t = performance.now(); const f = () => { n++; if (performance.now() - t < 10000) requestAnimationFrame(f); else res(n / ((performance.now() - t) / 1000)); }; requestAnimationFrame(f); }));
        log(`page rAF rate over 10 s: ${fps.toFixed(1)} fps`);
        await b.page.screenshot({ path: path.join(out, 'world-after.png') });
    } else if (!status) { log('TIMEOUT waiting for the world'); status = 2; }
    const mem = await b.page.evaluate(() => { try { return performance.memory ? { usedJSHeap: performance.memory.usedJSHeapSize } : null; } catch (_) { return null; } });
    log('page memory: ' + JSON.stringify(mem));
    if (query.includes('fnweb_heaptrack=1')) {
        const rep = await b.page.evaluate(() => window.__fnweb ? window.__fnweb.fnweb_heap_report(60) : 'no module');
        fs.writeFileSync(path.join(out, 'heap-sites.txt'), rep);
        log('heap by call site:\n' + rep.split('\n').slice(0, 30).join('\n'));
    }
} catch (e) {
    log('harness error: ' + (e.stack || e)); status = 3;
} finally {
    save();
    await b.page.screenshot({ path: path.join(out, 'final.png') }).catch(() => {});
    await b.close();
    server.proc.kill();
}
const errors = b.lines.filter((l) => /error|panicked|FATAL|exception/i.test(l));
log(`console lines: ${b.lines.length}, error-looking lines: ${errors.length}`);
for (const e of errors.slice(0, 30)) console.log('  ' + e.slice(0, 300));
process.exit(status);
