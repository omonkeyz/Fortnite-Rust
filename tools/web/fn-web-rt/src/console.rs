//! stdout / stderr -> browser console. Bytes are collected per thread until a newline; the main thread prints its lines directly, worker
//! lines are queued and printed by the main thread ([`drain`], called by the page every 50 ms): a worker that never returns to its event
//! loop (all pool workers) cannot rely on its own console reaching DevTools.
use std::cell::RefCell;
use std::sync::Mutex;

thread_local! {
    static LINE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// (level 0 = log / 2 = error, text) lines from workers, printed by the main thread.
static QUEUE: Mutex<Vec<(u32, String)>> = Mutex::new(Vec::new());
/// Every line printed so far is also kept (bounded) for the page's error overlay / test harness.
static HISTORY: Mutex<Vec<String>> = Mutex::new(Vec::new());
const HISTORY_MAX: usize = 4000;

fn emit(level: u32, text: String) {
    if let Ok(mut h) = HISTORY.try_lock() {
        if h.len() >= HISTORY_MAX { h.drain(..HISTORY_MAX / 4); }
        h.push(text.clone());
    }
    if crate::is_main_thread() {
        if level >= 2 { drain(); crate::js::fnweb_console(level, &text); } else if let Ok(mut q) = QUEUE.lock() { q.push((level, text)); }
    } else {
        // an error / panic line also goes to the worker's own console at once: when the main thread is the one stuck (waiting for this very
        // worker), the queue below is never drained
        if level >= 2 { crate::js::fnweb_console(level, &format!("[worker] {text}")); }
        if let Ok(mut q) = QUEUE.lock() { q.push((level, text)); }
    }
}

/// The std hook body: `fd` 1 = stdout, 2 = stderr.
pub fn write(fd: u32, buf: &[u8]) {
    let level = if fd == 2 { 2 } else { 0 };
    // stderr is the game's normal log (eprintln!): only lines that look like failures are console errors
    let _ = LINE.try_with(|l| {
        let Ok(mut l) = l.try_borrow_mut() else { return };
        l.extend_from_slice(buf);
        while let Some(i) = l.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = l.drain(..=i).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            let lvl = if level == 2 && looks_like_error(&text) { 2 } else { 0 };
            emit(lvl, text);
        }
    });
}

fn looks_like_error(t: &str) -> bool {
    let l = t.trim_start();
    l.starts_with("error") || l.starts_with("[wgpu error]") || l.contains("panicked at") || l.starts_with("thread '")
}

/// Main thread: prints the lines the workers queued. Returns how many.
pub fn drain() -> usize {
    let lines = match QUEUE.try_lock() { Ok(mut q) => std::mem::take(&mut *q), Err(_) => return 0 };
    let n = lines.len();
    let mut batch = String::new();
    let mut batch_level = 0;
    for (level, text) in lines {
        if !batch.is_empty() && (level != batch_level || batch.len() > 64 << 10) { crate::js::fnweb_console(batch_level, &batch); batch.clear(); }
        if !batch.is_empty() { batch.push(char::from(10u8)); }
        batch_level = level;
        batch.push_str(&text);
    }
    if !batch.is_empty() { crate::js::fnweb_console(batch_level, &batch); }
    n
}

/// The last `n` printed lines (all threads), oldest first.
pub fn tail(n: usize) -> Vec<String> {
    let h = HISTORY.lock().map(|h| h.clone()).unwrap_or_default();
    h[h.len().saturating_sub(n)..].to_vec()
}
