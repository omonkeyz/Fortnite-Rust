// Fortnite 1.8 browser build: the service worker of REMOTE MODE (the page found fnweb-remote.json beside it: the game files come from the
// user's private file Worker, not from the local launcher).
//
// The game's file system (fn-web-rt vfs.rs) requests <page dir>/__fnvfs/<op>?p=<path>[&v=<content version>] on the page's own origin, with the
// login's bearer token in the Authorization header. This worker answers those requests:
// * file contents (`file`, `bundle`): from Cache Storage when the same (op, path, content version, range) was fetched before, else from the file
//   Worker, and the reply is kept. `v` (the file's size + upload time from the bucket listing, see vfs.rs `version_of`) makes the cache persistent
//   across launches AND correct when files in the bucket are replaced or added: a changed file gets a new key, everything else is read from this
//   browser's disk (a returning player re-downloads only what changed; no cache-version bump needed for asset uploads). Requests without `v` (an
//   old page) are keyed as before.
// * listings (`tree`, `list`, `stat`): network first (a new upload must show up on the next launch), the cached reply only when the Worker cannot
//   be reached.
// * the same request twice at once (the start-up prefetch, www/fnweb-prefetch.js, and the game asking for the same range) is fetched once.
// To the Worker the path goes in the X-Fnvfs-Path header and `v` is dropped, so every request of one op has ONE URL (one CORS preflight, cached
// 2 h, instead of one per file); the token and the Range header are passed on. POST <page dir>/__fnvfs/login goes to the Worker's /login.
// Nothing else of the page is touched. The token is never written into a URL or a cache key; cached replies live in this browser profile, for
// this origin, until `cacheVersion` in fnweb-remote.json changes (only needed to force a full re-download).
let configPromise = null;
const config = () => configPromise ??= fetch(new URL('fnweb-remote.json', self.registration.scope), { cache: 'no-store' }).then(async (r) => {
    if (!r.ok) throw new Error('fnweb-remote.json: HTTP ' + r.status);
    const cfg = await r.json();
    // one Cache Storage cache per cacheVersion; the replies of other versions are dropped
    cfg.cacheName = 'fnweb-files-v' + (cfg.cacheVersion || 1);
    for (const k of await caches.keys()) if (k.startsWith('fnweb-files') && k !== cfg.cacheName) await caches.delete(k);
    return cfg;
}).catch((e) => { configPromise = null; throw e; });

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

self.addEventListener('fetch', (e) => {
    const u = new URL(e.request.url);
    if (u.origin !== self.location.origin) return;
    const i = u.pathname.indexOf('/__fnvfs/');
    if (i < 0) return;
    e.respondWith(handle(e.request, u, u.pathname.slice(i + '/__fnvfs/'.length)));
});

// a header value must be a byte string; game paths are ASCII (anything else goes in the query, as the Worker also accepts)
const asciiOnly = (s) => /^[\x20-\x7e]*$/.test(s);
const METADATA = new Set(['tree', 'list', 'stat']);
// in-flight fetches by cache key -> Promise<{ status, statusText, body }>
const inflight = new Map();
const net = { requests: 0, bytes: 0, hits: 0, byOp: {} };
let linkFree = 0;
let gameInflight = 0;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function throttle(cfg, phase, n, t0) {
    const t = cfg.throttle;
    if (!t) return;
    if (phase === 'rtt') { if (t.rttMs > 0) await sleep(t.rttMs); return; }
    if (!(t.mbps > 0)) return;
    const now = performance.now();
    const start = Math.max(t0 || now, linkFree);
    linkFree = start + n * 8 / (t.mbps * 1000);
    if (linkFree > now) await sleep(linkFree - now);
}
// listings fetched in the last minute (the prefetch worker reads the trees just before the game does): key -> { t, x }
const recent = new Map();
const RECENT_MS = 60000;
let cacheHandle = null;
const openCache = (name) => (cacheHandle && cacheHandle.name === name) ? cacheHandle.p : (cacheHandle = { name, p: caches.open(name) }).p;
async function readSlice(r, a, b) {
    const out = new Uint8Array(b - a + 1);
    const reader = r.body.getReader();
    let pos = 0, got = 0;
    for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        const lo = Math.max(a, pos), hi = Math.min(b + 1, pos + value.byteLength);
        if (hi > lo) { out.set(value.subarray(lo - pos, hi - pos), lo - a); got += hi - lo; }
        pos += value.byteLength;
        if (pos > b) { reader.cancel().catch(() => {}); break; }
    }
    return got === out.byteLength ? out.buffer : out.slice(0, got).buffer;
}
async function fetchRetry(target, opts, bytes, slice) {
    const ms = 20000 + Math.ceil((bytes || 0) / 1048576) * 1000;
    for (let k = 0; ; k++) {
        const ac = new AbortController();
        const timer = setTimeout(() => ac.abort(), ms);
        try {
            const r = await fetch(target, { ...opts, signal: ac.signal });
            if (slice && r.status === 200 && r.body) return { r, body: await readSlice(r, slice[0], slice[1]), sliced: true };
            const body = await r.arrayBuffer();
            return { r, body };
        } catch (e) {
            if (k >= 3 || !ac.signal.aborted) throw e;
            console.warn('[fnweb sw] ' + target + ': no full answer in ' + (ms / 1000) + ' s, asking again');
        } finally { clearTimeout(timer); }
    }
}
async function streamRun(cfg, target, opts, start, total, parts, onPart) {
    const ms = 20000 + Math.ceil(total / 1048576) * 1000;
    for (let k = 0; ; k++) {
        const ac = new AbortController();
        const timer = setTimeout(() => ac.abort(), ms);
        try {
            const r = await fetch(target, { ...opts, signal: ac.signal });
            if (r.status !== 206 && r.status !== 200) throw new Error('HTTP ' + r.status);
            let pos = r.status === 200 ? 0 : start;
            const open = parts.filter((p) => !p.done);
            const reader = r.body.getReader();
            for (;;) {
                const t0 = performance.now();
                const { value, done } = await reader.read();
                if (done) break;
                net.bytes += value.byteLength;
                await throttle(cfg, 'bw', value.byteLength, t0);
                const end = pos + value.byteLength;
                for (const p of open) {
                    if (p.done) continue;
                    const lo = Math.max(p.a, pos), hi = Math.min(p.b + 1, end);
                    if (hi <= lo) continue;
                    if (!p.buf) { p.buf = new Uint8Array(p.b - p.a + 1); p.got = 0; }
                    p.buf.set(value.subarray(lo - pos, hi - pos), lo - p.a);
                    p.got += hi - lo;
                    if (p.got === p.buf.byteLength) { p.done = true; const ab = p.buf.buffer; p.buf = null; onPart(p, ab); }
                }
                pos = end;
                if (open.every((p) => p.done)) { reader.cancel().catch(() => {}); break; }
            }
            return;
        } catch (e) {
            for (const p of parts) if (!p.done) p.buf = null;
            if (k >= 3 || !ac.signal.aborted) throw e;
            console.warn('[fnweb sw] ' + target + ': no full answer in ' + (ms / 1000) + ' s, asking again');
        } finally { clearTimeout(timer); }
    }
}
const reply = (x) => new Response(x.body, { status: x.status === 206 ? 200 : x.status, statusText: x.statusText, headers: { 'Content-Type': 'application/octet-stream', 'Content-Length': String(x.body.byteLength) } });

async function handle(req, u, op, retried = false) {
    let cfg;
    try { cfg = await config(); } catch (e) { return new Response(String(e), { status: 503 }); }
    const worker = String(cfg.worker || '').replace(/\/+$/, '');
    if (op === 'swstats') return new Response(JSON.stringify(net), { headers: { 'Content-Type': 'application/json' } });
    if (op === 'login') {
        return fetch(worker + '/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: await req.text(), cache: 'no-store' });
    }
    if (req.method !== 'GET' && req.method !== 'HEAD') return new Response('read-only', { status: 405 });
    const path = u.searchParams.get('p') || '';
    const version = u.searchParams.get('v');
    const rest = new URLSearchParams(u.searchParams);
    rest.delete('p');
    rest.delete('v');
    const range = req.headers.get('Range') || '';
    // the range is part of the query: Cache Storage ignores a URL's fragment when it matches
    const key = `https://fnweb-cache.invalid/${op}${path.startsWith('/') ? '' : '/'}${encodeURI(path)}?${rest}&range=${encodeURIComponent(range)}${version ? '&v=' + encodeURIComponent(version) : ''}`;
    const cache = await openCache(cfg.cacheName);
    const metadata = METADATA.has(op);
    // the start-up prefetch (www/fnweb-prefetch.js): fill the cache, send no body back
    const prefetch = req.headers.get('X-Fnvfs-Prefetch') === '1';
    if (!metadata) {
        const hit = await cache.match(key);
        if (hit) { net.hits++; return prefetch ? new Response(null, { status: 204 }) : hit; }
    } else {
        const r = recent.get(key);
        if (r && performance.now() - r.t < RECENT_MS) return reply(r.x);
    }
    // COALESCED PREFETCH: one run of neighbouring ranges of one file (X-Fnvfs-Parts = the game's own ranges "a-b,c-d,..." inside the run's Range) is
    // fetched with ONE request and cut into the per-read cache entries the game asks for; each part is in flight meanwhile (a game read joins it)
    const parts = prefetch && op === 'file' && req.headers.get('X-Fnvfs-Parts');
    if (parts) {
        const keyOf = (a, b) => `https://fnweb-cache.invalid/${op}${path.startsWith('/') ? '' : '/'}${encodeURI(path)}?${rest}&range=${encodeURIComponent(`bytes=${a}-${b}`)}${version ? '&v=' + encodeURIComponent(version) : ''}`;
        const want = [];
        for (const s of parts.split(',')) { const [a, b] = s.split('-').map(Number); const k = keyOf(a, b); if (!inflight.has(k) && !(await cache.match(k))) want.push([a, b, k]); }
        if (!want.length) return new Response(null, { status: 204 });
        const m = /^bytes=(\d+)-(\d+)$/.exec(range); if (!m) return new Response('bad run range', { status: 400 });
        const start = Number(m[1]);
        const ps = want.map(([a, b, k]) => { let res; const settled = new Promise((r) => { res = r; }); return { a, b, k, res, done: false, settled }; });
        let ok = true;
        const pending = ps.map((p) => {
            const pp = p.settled.then(async (x) => { if (!x.failed) { try { await cache.put(p.k, reply(x)); } catch (_) {} } return x; }).finally(() => inflight.delete(p.k));
            inflight.set(p.k, pp);
            return pp;
        });
        (async () => {
            const headers = new Headers({ Range: range });
            const auth = req.headers.get('Authorization'); if (auth) headers.set('Authorization', auth);
            let target = `${worker}/__fnvfs/file`;
            if (asciiOnly(path)) headers.set('X-Fnvfs-Path', path); else target += '?p=' + encodeURIComponent(path);
            for (let w = 0; gameInflight > 0 && w < 50; w++) await sleep(20);
            net.requests++; net.byOp.run = (net.byOp.run || 0) + 1;
            await throttle(cfg, 'rtt');
            await streamRun(cfg, target, { headers, cache: 'no-store', mode: 'cors', priority: 'low' }, start, Number(m[2]) - start + 1, ps, (p, body) => p.res({ status: 200, statusText: 'OK', body }));
        })().catch((e) => { ok = false; for (const p of ps) if (!p.done) { p.done = true; p.res({ status: 502, statusText: String(e), body: new ArrayBuffer(0), failed: true }); } })
            .finally(() => { for (const p of ps) if (!p.done) { ok = false; p.done = true; p.res({ status: 502, statusText: 'short answer', body: new ArrayBuffer(0), failed: true }); } });
        await Promise.all(pending);
        return new Response(null, { status: ok ? 204 : 502 });
    }
    // one network fetch per key at a time (the start-up prefetch and the game may ask for the same range together)
    let p = inflight.get(key);
    if (!p) {
        p = (async () => {
            const headers = new Headers();
            const auth = req.headers.get('Authorization');
            if (auth) headers.set('Authorization', auth);
            if (range) headers.set('Range', range);
            let target = `${worker}/__fnvfs/${op}`;
            const q = new URLSearchParams(rest);
            if (asciiOnly(path)) headers.set('X-Fnvfs-Path', path); else q.set('p', path);
            if (q.toString()) target += '?' + q;
            let r;
            net.requests++; net.byOp[op] = (net.byOp[op] || 0) + 1;
            const urgent = !prefetch && !metadata;
            if (urgent) gameInflight++;
            let x, t0 = 0;
            try {
                await throttle(cfg, 'rtt');
                t0 = performance.now();
                const rm0 = /^bytes=(\d+)-(\d+)$/.exec(range);
                let got;
                try { got = await fetchRetry(target, { headers, cache: 'no-store', mode: 'cors', priority: urgent ? 'high' : 'low' }, rm0 ? Number(rm0[2]) - Number(rm0[1]) + 1 : 100 << 20, rm0 ? [Number(rm0[1]), Number(rm0[2])] : null); }
                catch (e) { return { status: 502, statusText: 'file Worker unreachable', body: new TextEncoder().encode('file Worker unreachable: ' + e).buffer, failed: true }; }
                r = got.r;
                x = { status: r.status, statusText: r.statusText, body: got.body, sliced: !!got.sliced };
            } finally { if (urgent) gameInflight--; }
            net.bytes += x.body.byteLength;
            await throttle(cfg, 'bw', x.body.byteLength, t0);
            const rm = r.status === 200 && !x.sliced && /^bytes=(\d+)-(\d+)$/.exec(range);
            if (rm) x = { status: 200, statusText: r.statusText, body: x.body.slice(Number(rm[1]), Number(rm[2]) + 1) };
            // kept as a 200 (Cache Storage refuses 206); the game's file system takes either and reads the bytes. The in-flight entry stays until
            // the cache holds the reply, so a request in between cannot miss both.
            if (r.status === 200 || r.status === 206) { try { await cache.put(key, reply(x)); } catch (_) {} }
            else if (r.status >= 500) x.failed = true;
            return x;
        })().finally(() => inflight.delete(key));
        inflight.set(key, p);
    }
    const x = await p;
    // a read that joined a prefetch run which failed: fetch it on its own once
    if (x.failed && !metadata && !prefetch && !retried) return handle(req, u, op, true);
    if (metadata && !x.failed && x.status === 200) recent.set(key, { t: performance.now(), x });
    if (prefetch && !metadata) return new Response(null, { status: x.status === 200 || x.status === 206 ? 204 : x.status });
    // a listing the Worker could not give (offline, 5xx): the one cached by an earlier launch
    if (metadata && x.failed) { const hit = await cache.match(key); if (hit) return hit; }
    return reply(x);
}
