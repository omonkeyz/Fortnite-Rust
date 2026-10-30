//! The Web Worker pool behind `std::thread`.
//!
//! The page starts N workers before it calls into Rust (a Worker created later only starts once the main thread returns to the event loop, so a
//! spawn-then-join on the main thread would deadlock otherwise). Each worker instantiates the same module on the same shared memory and parks in
//! [`fnweb_worker_main`] on a futex (std `Condvar`), not on `onmessage`. `std::thread::spawn` (patched std, `sys/thread/fnweb.rs`) hands the
//! thread's start routine to [`spawn_job`]; an idle worker runs it. When idle workers run short the pool asks the page for more (they join
//! after the main thread yields).
//!
//! Growth: the pool keeps a reserve of idle workers (it asks for 4 more whenever idle + requested workers fall below the queue + 2). The reserve
//! matters: a job the MAIN thread waits for (its file reads are made by workers) must find a worker at once, because while the main thread waits no
//! new worker can start, and workers can be taken for good (rayon's pool, the audio stream thread) or blocked on jobs still queued (nested
//! `thread::scope`s). A pool that stopped growing at 40 deadlocked that way on the first loading step (verification/browser/launch6), so the cap
//! ([`soft_cap`], 96) is only a guard against runaway growth; every worker costs its stack in the wasm heap (2 MiB, `WORKER_STACK` in
//! fnweb-boot.js) and the pool never shrinks. Past the cap only a queue that makes NO progress (jobs waiting, no worker idle, no job started for two
//! 50 ms ticks of [`tick`]) adds workers.
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use wasm_bindgen::prelude::*;

thread_local! {
    static CAN_BLOCK: Cell<bool> = const { Cell::new(false) };
    static WORKER_INDEX: Cell<i32> = const { Cell::new(-1) };
}

static QUEUE: Mutex<VecDeque<usize>> = Mutex::new(VecDeque::new());
static WAKE: Condvar = Condvar::new();
static IDLE: AtomicUsize = AtomicUsize::new(0);
static WORKERS: AtomicUsize = AtomicUsize::new(0);
static STARTED_JOBS: AtomicUsize = AtomicUsize::new(0);
/// Workers requested from the page but not yet running.
static PENDING: AtomicUsize = AtomicUsize::new(0);
/// `available_parallelism()` reported to the program (the page may lower it; see [`set_parallelism`]).
static PARALLELISM: AtomicUsize = AtomicUsize::new(0);
/// Workers up to which a spawn may grow the pool (see the module docs). `?fnweb_pool_cap=N` (page) overrides it.
static SOFT_CAP: AtomicUsize = AtomicUsize::new(DEFAULT_SOFT_CAP);
/// The normal load peaks at 32-36 workers (verification/browser/walk2, launch1, launch2) plus the audio's long-lived threads; a nested
/// texture-decode fan-out reached 57 (launch3). 40 deadlocked (launch6, see the module docs).
const DEFAULT_SOFT_CAP: usize = 96;
/// [`tick`]'s view of the queue: jobs started at the last tick, ticks in a row without progress, workers added past the cap.
static TICK_STARTED: AtomicUsize = AtomicUsize::new(0);
static STUCK_TICKS: AtomicUsize = AtomicUsize::new(0);
static OVER_CAP: AtomicUsize = AtomicUsize::new(0);

pub fn can_block() -> bool { CAN_BLOCK.try_with(|c| c.get()).unwrap_or(false) }

/// -1 on the main thread.
pub fn worker_index() -> i32 { WORKER_INDEX.try_with(|c| c.get()).unwrap_or(-1) }

pub fn stats() -> (usize, usize, usize, usize) {
    (WORKERS.load(Ordering::Relaxed), IDLE.load(Ordering::Relaxed), QUEUE.lock().map(|q| q.len()).unwrap_or(0), STARTED_JOBS.load(Ordering::Relaxed))
}

pub fn set_parallelism(n: usize) { PARALLELISM.store(n.max(1), Ordering::Relaxed); }

pub fn parallelism() -> usize {
    match PARALLELISM.load(Ordering::Relaxed) {
        0 => (crate::js::fnweb_hardware_concurrency() as usize).clamp(1, 8),
        n => n,
    }
}

pub fn soft_cap() -> usize { SOFT_CAP.load(Ordering::Relaxed) }

/// The page's `?fnweb_pool_cap=N` (at least 48).
#[wasm_bindgen]
pub fn fnweb_set_pool_cap(n: u32) { SOFT_CAP.store((n as usize).max(48), Ordering::Relaxed); }

fn request(n: usize) {
    PENDING.fetch_add(n, Ordering::SeqCst);
    crate::js::fnweb_request_workers(n as u32);
}

/// Entry of every pool worker (called by `worker.js` after `init`): never returns.
#[wasm_bindgen]
pub fn fnweb_worker_main(index: u32) {
    CAN_BLOCK.with(|c| c.set(true));
    WORKER_INDEX.with(|c| c.set(index as i32));
    WORKERS.fetch_add(1, Ordering::SeqCst);
    let _ = PENDING.try_update(Ordering::SeqCst, Ordering::SeqCst, |p| Some(p.saturating_sub(1)));
    loop {
        let job = {
            let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            IDLE.fetch_add(1, Ordering::SeqCst);
            while q.is_empty() {
                q = WAKE.wait(q).unwrap_or_else(|e| e.into_inner());
            }
            IDLE.fetch_sub(1, Ordering::SeqCst);
            q.pop_front().unwrap()
        };
        STARTED_JOBS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: produced by `Box::into_raw(Box::new(Box<dyn FnOnce()>))` in std's sys/thread/fnweb.rs; ownership moves here.
        let f = unsafe { Box::from_raw(job as *mut Box<dyn FnOnce()>) };
        f();
    }
}

/// Queues a std thread's start routine. Always succeeds (the job waits for a worker if none is idle).
///
/// # Safety
/// `job` must come from `Box::into_raw(Box::new(Box<dyn FnOnce()>))`.
pub unsafe fn spawn_job(job: *mut u8) -> bool {
    let queued = {
        let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(job as usize);
        q.len()
    };
    WAKE.notify_one();
    // keep a reserve of idle workers: ask for more when the queue outgrows them (up to the soft cap; past it see `tick`)
    let idle = IDLE.load(Ordering::SeqCst);
    let pending = PENDING.load(Ordering::SeqCst);
    let total = WORKERS.load(Ordering::SeqCst) + pending;
    let cap = soft_cap();
    if idle + pending < queued.min(8) + 2 && total < cap {
        request(4.min(cap - total));
    }
    true
}

/// Main thread, every 50 ms (the page's timer, through `fnweb_drain_log`): adds 2 workers when the queue is stuck (jobs waiting, no worker idle,
/// none requested, no job started for 2 ticks), at any pool size. Past the soft cap this is the only way the pool grows.
pub fn tick() {
    let started = STARTED_JOBS.load(Ordering::Relaxed);
    let progressed = TICK_STARTED.swap(started, Ordering::Relaxed) != started;
    let queued = QUEUE.lock().map(|q| q.len()).unwrap_or(0);
    let stuck = queued > 0 && !progressed && IDLE.load(Ordering::SeqCst) == 0 && PENDING.load(Ordering::SeqCst) == 0;
    if !stuck { STUCK_TICKS.store(0, Ordering::Relaxed); return; }
    if STUCK_TICKS.fetch_add(1, Ordering::Relaxed) + 1 < 2 { return; }
    STUCK_TICKS.store(0, Ordering::Relaxed);
    let n = 2;
    let over = OVER_CAP.fetch_add(n, Ordering::Relaxed) + n;
    request(n);
    eprintln!("[fnweb] pool: {queued} jobs waited 100 ms with all {} workers busy: +{n} (soft cap {}, {over} added by this check so far)", WORKERS.load(Ordering::Relaxed), soft_cap());
}
