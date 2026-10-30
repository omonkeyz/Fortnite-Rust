// Boot of the browser build: shared wasm memory, the main-thread instance, the Web Worker pool (started BEFORE any Rust code runs: a worker
// created later only starts once the main thread yields, and std::thread::spawn + join on the main thread would deadlock), the console drain.
import init, * as wasm from './pkg/__FNWEB_LIB__.js';

// bytes per pool thread: the 2 MiB Rust gives spawned threads natively
export const WORKER_STACK = 2 << 20;

export async function boot({ workers, onStatus = () => {}, onFatal = () => {} } = {}) {
    if (!crossOriginIsolated) throw new Error('the page is not cross-origin isolated (COOP/COEP headers missing): open it through fortnite-web.exe');
    if (!('gpu' in navigator)) throw new Error('this browser has no WebGPU (navigator.gpu is missing)');
    if (new URLSearchParams(location.search).has('fnweb_pipe_log') && globalThis.GPUDevice) {
        const P = GPUDevice.prototype;
        for (const m of ['createRenderPipeline', 'createComputePipeline', 'createRenderPipelineAsync', 'createComputePipelineAsync']) {
            const orig = P[m];
            P[m] = function (d) { const t = performance.now(); const r = orig.call(this, d); console.log('[pipe] ' + m + ' ' + (d && d.label || '?') + ' at ' + (t / 1000).toFixed(2) + ' s, ' + (performance.now() - t).toFixed(1) + ' ms'); return r; };
        }
    }
    // initial 1 GiB, may grow to the full 4 GiB wasm32 address space
    const memory = new WebAssembly.Memory({ initial: 16384, maximum: 65536, shared: true });
    onStatus('compiling the game module');
    const t0 = performance.now();
    // compile once; the same WebAssembly.Module instantiates the main thread and every worker
    const module = await WebAssembly.compileStreaming(fetch(new URL('./pkg/__FNWEB_LIB___bg.wasm', import.meta.url)));
    const compileMs = performance.now() - t0;
    await init({ module_or_path: module, memory });

    const io = new Worker(new URL('./worker.js', import.meta.url), { type: 'module', name: 'fnweb-io' });
    io.onmessage = (e) => { const d = e.data || {}; if (d.fnweb === 'error') onFatal(d.text); };
    io.onerror = (e) => { e.preventDefault?.(); onFatal('I/O worker crashed: ' + (e.message || e)); };
    io.postMessage({ fnweb: 'io', module, memory, stack: WORKER_STACK });
    const pool = [];
    let started = 0;
    const spawn = () => new Promise((resolve, reject) => {
        const index = pool.length;
        const w = new Worker(new URL('./worker.js', import.meta.url), { type: 'module', name: 'fnweb-' + index });
        pool.push(w);
        w.onmessage = (e) => {
            const d = e.data || {};
            if (d.fnweb === 'ready') { started++; resolve(w); }
            else if (d.fnweb === 'grow') grow(d.n);
            // remote mode: a worker's /fn/user write (it cannot use IndexedDB itself: it never returns to its event loop)
            else if (d.fnweb === 'usersave') { if (globalThis.__fnweb_user_save) globalThis.__fnweb_user_save(d.path, d.bytes); }
            else if (d.fnweb === 'userdelete') { if (globalThis.__fnweb_user_delete) globalThis.__fnweb_user_delete(d.path); }
            else if (d.fnweb === 'error') { onFatal('worker ' + index + ': ' + d.text); reject(new Error(d.text)); }
        };
        w.onerror = (e) => { e.preventDefault?.(); try { wasm.fnweb_drain_log(); } catch (_) {} onFatal('worker ' + index + ' crashed: ' + (e.message || e)); };
        const ch = new MessageChannel();
        io.postMessage({ fnweb: 'io-port', port: ch.port1 }, [ch.port1]);
        w.postMessage({ fnweb: 'start', module, memory, index, stack: WORKER_STACK, io: ch.port2 }, [ch.port2]);
    });
    const grow = (n) => { for (let i = 0; i < n; i++) spawn().catch(() => {}); console.log('[fnweb] pool: +' + n + ' workers (now ' + pool.length + ')'); };
    globalThis.__fnweb_grow_pool = grow;
    // the long-lived threads + the loaders' parallel pools; more are added on demand (each costs its stack in the wasm heap)
    const n = workers || Math.max(12, Math.min(24, (navigator.hardwareConcurrency || 8) + 4));
    onStatus('starting ' + n + ' worker threads');
    await Promise.all(Array.from({ length: n }, spawn));
    setInterval(() => { try { wasm.fnweb_drain_log(); } catch (_) {} }, 50);
    console.log('[fnweb] module compiled in ' + compileMs.toFixed(0) + ' ms, ' + started + ' pool workers ready');
    return { wasm, memory, module, pool, io };
}
