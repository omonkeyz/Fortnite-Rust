//! The `__fnweb_*` symbols the patched std calls (declarations: `library/std/src/sys/pal/wasm/fnweb.rs` in tools/web/std-patch). Signatures must
//! match exactly; both sides are compiled by the same rustc in the same `-Z build-std` build, so std types may cross.
use std::io;

#[no_mangle]
pub fn __fnweb_can_block() -> bool { crate::pool::can_block() }

#[no_mangle]
pub fn __fnweb_now_ms() -> f64 { crate::js::fnweb_now_ms() }

#[no_mangle]
pub fn __fnweb_wall_ms() -> f64 { crate::js::fnweb_wall_ms() }

#[no_mangle]
pub fn __fnweb_stdio_write(fd: u32, buf: &[u8]) { crate::console::write(fd, buf) }

#[no_mangle]
pub fn __fnweb_getenv(key: &str) -> Option<String> { crate::env::get(key) }

#[no_mangle]
pub fn __fnweb_setenv(key: &str, value: Option<&str>) { crate::env::set(key, value) }

#[no_mangle]
pub fn __fnweb_env_all() -> Vec<(String, String)> { crate::env::all() }

#[no_mangle]
pub fn __fnweb_args() -> Vec<String> { crate::env::args() }

/// # Safety
/// See [`crate::pool::spawn_job`].
#[no_mangle]
pub unsafe fn __fnweb_thread_spawn(job: *mut u8) -> bool { unsafe { crate::pool::spawn_job(job) } }

#[no_mangle]
pub fn __fnweb_parallelism() -> usize { crate::pool::parallelism() }

#[no_mangle]
pub fn __fnweb_fs_stat(path: &str) -> io::Result<(u8, u64, f64)> { crate::vfs::stat(path) }

#[no_mangle]
pub fn __fnweb_fs_readdir(path: &str) -> io::Result<Vec<(String, u8, u64, f64)>> { crate::vfs::readdir(path) }

#[no_mangle]
pub fn __fnweb_fs_open(path: &str, flags: u32) -> io::Result<u64> { crate::vfs::open(path, flags) }

#[no_mangle]
pub fn __fnweb_fs_read_at(h: u64, off: u64, buf: &mut [u8]) -> io::Result<usize> { crate::vfs::read_at(h, off, buf) }

#[no_mangle]
pub fn __fnweb_fs_write_at(h: u64, off: u64, buf: &[u8]) -> io::Result<usize> { crate::vfs::write_at(h, off, buf) }

#[no_mangle]
pub fn __fnweb_fs_size(h: u64) -> io::Result<u64> { crate::vfs::size(h) }

#[no_mangle]
pub fn __fnweb_fs_set_len(h: u64, len: u64) -> io::Result<()> { crate::vfs::set_len(h, len) }

#[no_mangle]
pub fn __fnweb_fs_flush(h: u64) -> io::Result<()> { crate::vfs::flush(h) }

#[no_mangle]
pub fn __fnweb_fs_close(h: u64) { crate::vfs::close(h) }

#[no_mangle]
pub fn __fnweb_fs_unlink(path: &str) -> io::Result<()> { crate::vfs::unlink(path) }

#[no_mangle]
pub fn __fnweb_fs_rename(from: &str, to: &str) -> io::Result<()> { crate::vfs::rename(from, to) }

#[no_mangle]
pub fn __fnweb_fs_mkdir(path: &str) -> io::Result<()> { crate::vfs::mkdir(path) }

#[no_mangle]
pub fn __fnweb_fs_rmdir(path: &str) -> io::Result<()> { crate::vfs::rmdir(path) }

/// Main thread, every 50 ms (page timer): prints the console lines the workers queued, and lets the worker pool check for a stuck queue
/// ([`crate::pool::tick`]).
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_drain_log() -> usize { crate::pool::tick(); crate::console::drain() }

/// Runtime counters for the page's status line: [workers, idle, queued jobs, jobs started, http requests, MB fetched, cache hits, cache MB,
/// live heap MB, peak heap MB, wasm memory MB, allocations, ms inside requests (all threads), ms the main thread waited for files, small files
/// served from bundles].
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_runtime_stats() -> Vec<f64> {
    let (w, i, q, j) = crate::pool::stats();
    let (r, b, h) = crate::vfs::stats();
    let (live, peak, mem, allocs) = crate::mem::stats();
    let mb = |x: usize| x as f64 / 1048576.0;
    let (http_ms, main_wait_ms, bundled) = crate::vfs::timing();
    vec![w as f64, i as f64, q as f64, j as f64, r as f64, b as f64 / 1048576.0, h as f64, mb(crate::vfs::cache_bytes()), mb(live), mb(peak), mb(mem), allocs as f64, http_ms, main_wait_ms, bundled as f64]
}

/// Resets the peak heap figure to the current live size.
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_reset_peak() { crate::mem::reset_peak() }

/// Sets an environment variable (page start-up: URL parameters, config).
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_setenv(key: &str, value: &str) { crate::env::set(key, Some(value)) }

/// Heap attribution (debug): start tracking allocations >= 1 MiB by call site.
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_heap_track(on: bool) { crate::mem::enable_tracking(on) }

/// Heap attribution (debug): the live tracked allocations grouped by call site, biggest first.
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_heap_report(top: usize) -> String { crate::mem::report(top) }

#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_heap_report_peak(top: usize) -> String { crate::mem::report_peak(top) }

/// REMOTE MODE (the page, after its login, before the game starts): read the game files through `prefix`/__fnvfs (the page's service worker)
/// with `token`; see vfs.rs.
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_vfs_remote(prefix: &str, token: &str) { crate::vfs::set_remote(prefix, token) }

/// REMOTE MODE: a /fn/user file saved by an earlier session (the page's IndexedDB), put back before the game starts.
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_io_done(id: u32, status: u32, body: Option<Vec<u8>>, err: String) { crate::vfs::io_done(id, status, body, err) }

#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_io_chunk(id: u32, chunk: &js_sys::Uint8Array) { crate::vfs::io_chunk(id, chunk) }

#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_vfs_tree(root: &str, text: &str) { crate::vfs::set_tree(root, text) }

#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_vfs_user_file(path: &str, bytes: Vec<u8>) { crate::vfs::preload_user_file(path, bytes) }

/// BOOT MANIFEST (`?FNWEB_RECORD=1`): the file / bundle requests of this session in first-request order (JSON; see vfs.rs `record`).
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_fetch_log() -> String { crate::vfs::fetch_log() }

/// TEST / DEBUG (browser console, test harness): `len` bytes of `path` at `off` through std::fs (the game's own path to the files).
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_vfs_test_read(path: &str, off: f64, len: u32) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))?;
    f.seek(SeekFrom::Start(off as u64)).map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))?;
    let mut buf = vec![0u8; len as usize];
    let mut done = 0;
    while done < buf.len() { match f.read(&mut buf[done..]) { Ok(0) => break, Ok(n) => done += n, Err(e) => return Err(wasm_bindgen::JsValue::from_str(&e.to_string())) } }
    buf.truncate(done);
    Ok(buf)
}

/// TEST / DEBUG: `kind size` of `path` and, for a directory, its entries ("name kind size" lines), through std::fs.
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn fnweb_vfs_test_list(path: &str) -> Result<String, wasm_bindgen::JsValue> {
    let m = std::fs::metadata(path).map_err(|e| wasm_bindgen::JsValue::from_str(&format!("{path}: {e}")))?;
    let mut out = format!("{} {}\n", if m.is_dir() { "dir" } else { "file" }, m.len());
    if m.is_dir() {
        for e in std::fs::read_dir(path).map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))?.flatten() {
            let md = e.metadata().ok();
            out.push_str(&format!("{} {} {}\n", e.file_name().to_string_lossy(), if md.as_ref().is_some_and(|m| m.is_dir()) { "dir" } else { "file" }, md.map_or(0, |m| m.len())));
        }
    }
    Ok(out)
}
