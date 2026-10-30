// fn-web-rt JS side: the few browser calls the Rust platform layer needs. Imported by the wasm-bindgen glue on the main thread AND in every
// pool worker (same module), so everything here must work in a Window and in a DedicatedWorkerGlobalScope.

const IS_WINDOW = typeof window !== 'undefined' && typeof document !== 'undefined';

export function fnweb_is_window() { return IS_WINDOW; }

// One clock origin for every realm: Instant::now() values cross threads.
export function fnweb_now_ms() { return performance.timeOrigin + performance.now(); }
export function fnweb_wall_ms() { return Date.now(); }

export function fnweb_console(level, text) {
    if (level >= 2) console.error(text); else if (level === 1) console.warn(text); else console.log(text);
}

// Synchronous HTTP. Workers can ask for an ArrayBuffer; a Window may not set responseType on a synchronous request (InvalidAccessError), so the
// main thread reads the body as x-user-defined text and converts each char back to its byte.
// Returns: null for 404, else a Uint8Array (status 200 / 206). Throws an Error with the status text for anything else.
export function fnweb_io_port() { return !!globalThis.__fnwebIoPort; }
export function fnweb_io_request(id, url, rangeStart, rangeEnd, token, high) {
    globalThis.__fnwebIoPort.postMessage({ id, url, a: rangeStart, b: rangeEnd, token: token || null, hi: !!high });
}

let httpXhr = null;
let httpLast = null;
export function fnweb_http(method, url, rangeStart, rangeEnd, body, token) {
    if (httpLast) { try { if (httpLast.transfer && !httpLast.detached) httpLast.transfer(0); } catch (_) {} httpLast = null; }
    const x = httpXhr || (httpXhr = new XMLHttpRequest());
    x.open(method, url, false);
    if (rangeStart >= 0) x.setRequestHeader('Range', 'bytes=' + rangeStart + '-' + rangeEnd);
    // remote mode: the login's bearer token (a header: never part of a URL)
    if (token) x.setRequestHeader('Authorization', 'Bearer ' + token);
    if (IS_WINDOW) x.overrideMimeType('text/plain; charset=x-user-defined');
    else x.responseType = 'arraybuffer';
    // a view on the shared wasm memory cannot be sent: copy it
    x.send(body == null ? null : body.slice());
    if (x.status === 404) return null;
    if (x.status !== 200 && x.status !== 206 && x.status !== 204) throw new Error('HTTP ' + x.status + ' ' + method + ' ' + url + ': ' + (IS_WINDOW ? x.responseText.slice(0, 200) : ''));
    if (IS_WINDOW) {
        const s = x.responseText;
        const n = s.length;
        const out = new Uint8Array(n);
        for (let i = 0; i < n; i++) out[i] = s.charCodeAt(i) & 0xff;
        httpLast = out.buffer;
        return out;
    }
    const ab = x.response || new ArrayBuffer(0);
    httpLast = ab;
    return new Uint8Array(ab);
}

export function fnweb_hardware_concurrency() {
    return (typeof navigator !== 'undefined' && navigator.hardwareConcurrency) || 4;
}

// Main thread only: ask the page to start more pool workers (they become available after the main thread yields).
export function fnweb_request_workers(n) {
    if (IS_WINDOW) { if (globalThis.__fnweb_grow_pool) globalThis.__fnweb_grow_pool(n); }
    else { self.postMessage({ fnweb: 'grow', n }); }
}

// URL query parameters (main thread) -> [[k, v], ...]
export function fnweb_url_params() {
    if (!IS_WINDOW) return [];
    const out = [];
    for (const [k, v] of new URLSearchParams(location.search)) out.push([k, v]);
    return out;
}

// heap attribution (debug): the current call stack, wasm frames only, as compact text
export function fnweb_stack() {
    const lim = Error.stackTraceLimit; Error.stackTraceLimit = 22;
    const s = new Error().stack || '';
    Error.stackTraceLimit = lim;
    const out = [];
    for (const line of s.split('\n')) {
        const m = line.match(/at (.*?) \(wasm:/) || line.match(/at (.*?) \(.*wasm-function/);
        if (!m) continue;
        let f = m[1].replace(/\[[0-9a-f]{16}\]/g, '').replace(/^<|>$/g, '');
        if (/fn_web_rt::mem|__rust_alloc|__rdl_|alloc::raw_vec|alloc::alloc|RawVec|finish_grow|try_allocate|with_capacity|reserve|__rustc::/.test(f)) continue;
        out.push(f.slice(0, 120));
        if (out.length >= 9) break;
    }
    return out.join(' < ');
}

export function fnweb_stack_deep(n) {
    const lim = Error.stackTraceLimit; Error.stackTraceLimit = Math.max(lim || 10, n + 20);
    const s = new Error().stack || '';
    Error.stackTraceLimit = lim;
    const out = [];
    for (const line of s.split('\n')) {
        const m = line.match(/at (.*?) \(wasm:/) || line.match(/at (.*?) \(.*wasm-function/);
        if (!m) continue;
        let f = m[1].replace(/\[[0-9a-f]{16}\]/g, '').replace(/^<|>$/g, '');
        if (/^_ZN/.test(f)) { const parts = []; let r = f.slice(3); while (/^\d/.test(r)) { const k = r.match(/^(\d+)/); const len = Number(k[1]); r = r.slice(k[1].length); parts.push(r.slice(0, len)); r = r.slice(len); } if (parts.length && /^h[0-9a-f]{16}$/.test(parts[parts.length - 1])) parts.pop(); f = parts.join('::').replace(/\$LT\$/g, '<').replace(/\$GT\$/g, '>').replace(/\$u20\$/g, ' ').replace(/\$RF\$/g, '&').replace(/\$C\$/g, ',').replace(/\.\./g, '::'); }
        if (/fn_web_rt::|^<?std::|^<?core::|__fnweb|^<?alloc::|__rustc::|fn_pak::|fn_fx::webcook|fn_fx::webtex|LocalKey|fn_fx::data::|fn_ueio::/.test(f)) continue;
        out.push(f.slice(0, 140));
        if (out.length >= n) break;
    }
    return out.join(' < ');
}

export function fnweb_page_origin() {
    return (typeof location !== 'undefined') ? location.origin : '';
}

// REMOTE MODE: /fn/user files live in the page's IndexedDB. A pool worker never returns to its event loop (it runs Rust jobs), so it cannot use
// IndexedDB itself: it posts the write to the page (fnweb-boot.js handles 'usersave' / 'userdelete'). The bytes are copied (shared memory changes).
export function fnweb_user_save(path, bytes) {
    const copy = bytes.slice();
    if (IS_WINDOW) { if (globalThis.__fnweb_user_save) globalThis.__fnweb_user_save(path, copy); }
    else self.postMessage({ fnweb: 'usersave', path, bytes: copy });
}
export function fnweb_user_delete(path) {
    if (IS_WINDOW) { if (globalThis.__fnweb_user_delete) globalThis.__fnweb_user_delete(path); }
    else self.postMessage({ fnweb: 'userdelete', path });
}
