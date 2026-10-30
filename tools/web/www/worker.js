// A pool worker: instantiate the same module on the shared memory, report ready, then run std threads' jobs forever (fnweb_worker_main never
// returns; it parks on a futex between jobs). See tools/web/fn-web-rt/src/pool.rs.
import init, * as wasm from './pkg/__FNWEB_LIB__.js';

const ioPorts = [];
const IO_BUF = 256 << 10;
const ioBufs = [];
let ioReady = false;
const ioLive = new Map();
let ioDone = 0;
setInterval(() => {
    const now = performance.now();
    for (const [k, v] of ioLive) if (now - v.t > 10000 && !v.logged) { v.logged = true; console.warn('[fnweb io] worker: request ' + k + ' still ' + v.stage + ' after ' + ((now - v.t) / 1000).toFixed(1) + ' s (' + ioLive.size + ' open, ' + ioDone + ' answered): ' + v.url + ' ' + v.a + '-' + v.b); }
}, 1000);
async function ioServe(m) {
    let status = 0, body, err = '';
    const key = m.id + ':' + performance.now().toFixed(0);
    const rec = { t: performance.now(), stage: 'fetching', url: m.url, a: m.a, b: m.b };
    ioLive.set(key, rec);
    try {
        const headers = {};
        if (m.a >= 0) headers.Range = 'bytes=' + m.a + '-' + m.b;
        if (m.token) headers.Authorization = 'Bearer ' + m.token;
        const r = await fetch(m.url, { headers, priority: m.hi ? 'high' : 'low' });
        rec.stage = 'reading body (' + r.status + ')';
        status = r.status;
        if (status === 200 || status === 206 || status === 204) {
            let reader = null;
            if (r.body && m.a >= 0 && wasm.fnweb_io_chunk) { try { reader = r.body.getReader({ mode: 'byob' }); } catch (_) { reader = null; } }
            if (!reader) body = new Uint8Array(await r.arrayBuffer());
            else {
                let buf = ioBufs.pop() || new ArrayBuffer(IO_BUF);
                try {
                    for (;;) {
                        const { value, done } = await reader.read(new Uint8Array(buf));
                        if (value) { buf = value.buffer; if (value.byteLength) wasm.fnweb_io_chunk(m.id, value); }
                        if (done) break;
                    }
                } finally { if (buf.byteLength === IO_BUF) ioBufs.push(buf); }
            }
        }
        else if (status !== 404) err = (await r.text().catch(() => '')).slice(0, 200) + ' GET ' + m.url;
    } catch (x) { status = 0; err = String(x && x.message || x) + ' GET ' + m.url; }
    rec.stage = 'answering';
    try { wasm.fnweb_io_done(m.id, status, body, err); } catch (x) { console.error('[fnweb io] answer ' + m.id + ' failed: ' + (x && x.stack || x)); }
    ioLive.delete(key);
    ioDone++;
    if (rec.logged) console.warn('[fnweb io] worker: request ' + m.id + ' answered after ' + ((performance.now() - rec.t) / 1000).toFixed(1) + ' s, status ' + status);
}
const ioAttach = (port) => { port.onmessage = (e) => { ioServe(e.data); }; };

self.onmessage = async (e) => {
    const d = e.data || {};
    if (d.fnweb === 'io-port') { if (ioReady) ioAttach(d.port); else ioPorts.push(d.port); return; }
    if (d.fnweb === 'io') {
        try { await init({ module_or_path: d.module, memory: d.memory, thread_stack_size: d.stack }); }
        catch (err) { self.postMessage({ fnweb: 'error', text: 'I/O worker init failed: ' + (err && err.stack || err) }); return; }
        ioReady = true;
        for (const p of ioPorts.splice(0)) ioAttach(p);
        return;
    }
    if (d.fnweb !== 'start') return;
    if (d.io) globalThis.__fnwebIoPort = d.io;
    try {
        await init({ module_or_path: d.module, memory: d.memory, thread_stack_size: d.stack });
    } catch (err) {
        self.postMessage({ fnweb: 'error', text: 'init failed: ' + (err && err.stack || err) });
        return;
    }
    self.postMessage({ fnweb: 'ready', index: d.index });
    wasm.fnweb_worker_main(d.index);
};
