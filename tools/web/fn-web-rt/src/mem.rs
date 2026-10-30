//! Heap accounting of the browser build: a counting wrapper around std's allocator (dlmalloc) that tracks live and peak bytes, plus the size of the
//! wasm linear memory (it only grows: freed heap is reused by dlmalloc but never returned to the browser).
//!
//! Attribution (debug, `?fnweb_heaptrack=1`): every allocation of at least [`TRACK_MIN`] bytes records the wasm call stack of its site; the live ones
//! can be listed grouped by site ([`report`]). The tracker's own allocations bypass it (a per-thread guard), and it never holds its lock while
//! the allocator's lock is held. Smaller allocations are SAMPLED (one address in [`SAMPLE`], chosen by a hash of the address, so a free knows
//! whether its block was sampled without a lookup) and reported with that weight: most of a loaded heap is small objects.
use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicUsize, Ordering};
use std::sync::Mutex;

pub struct Counting;

static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

pub const TRACK_MIN: usize = 64 << 10;
static TRACKING: AtomicBool = AtomicBool::new(false);
/// ptr -> (size x weight, site id); site id -> stack text
static TRACKED: Mutex<Option<(HashMap<usize, (usize, u64)>, HashMap<u64, String>)>> = Mutex::new(None);
/// Allocations below [`TRACK_MIN`]: one address in this many is tracked (weight SAMPLE).
const SAMPLE: usize = 64;
#[inline]
fn sampled(p: *mut u8) -> bool { TRACKING.load(Ordering::Relaxed) && ((p as usize as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) == 0 }
#[inline]
fn watched(p: *mut u8, size: usize) -> bool { size >= TRACK_MIN || sampled(p) }
#[inline]
fn weight(size: usize) -> usize { if size >= TRACK_MIN { size } else { size * SAMPLE } }

thread_local! { static IN_TRACKER: Cell<bool> = const { Cell::new(false) }; }

pub fn enable_tracking(on: bool) { TRACKING.store(on, Ordering::Relaxed); }

fn hash(s: &str) -> u64 { s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3)) }

fn track(p: *mut u8, size: usize) {
    if !TRACKING.load(Ordering::Relaxed) { return; }
    let _ = IN_TRACKER.try_with(|g| {
        if g.replace(true) { return; }
        let stack = crate::js::fnweb_stack();
        let id = hash(&stack);
        if let Ok(mut t) = TRACKED.lock() {
            let (live, sites) = t.get_or_insert_with(|| (HashMap::new(), HashMap::new()));
            live.insert(p as usize, (weight(size), id));
            sites.entry(id).or_insert(stack);
        }
        g.set(false);
    });
}

fn untrack(p: *mut u8) {
    if !TRACKING.load(Ordering::Relaxed) { return; }
    let _ = IN_TRACKER.try_with(|g| {
        if g.replace(true) { return; }
        if let Ok(mut t) = TRACKED.lock() { if let Some((live, _)) = t.as_mut() { live.remove(&(p as usize)); } }
        g.set(false);
    });
}

thread_local! {
    static DELTA: Cell<i64> = const { Cell::new(0) };
    static NALLOC: Cell<u32> = const { Cell::new(0) };
}
const FLUSH_BYTES: i64 = 256 << 10;
const FLUSH_ALLOCS: u32 = 256;

fn flush(d: i64, n: u32) {
    if n > 0 { ALLOCS.fetch_add(n as usize, Ordering::Relaxed); }
    let live = LIVE.fetch_add(d, Ordering::Relaxed) + d;
    let mut p = PEAK.load(Ordering::Relaxed);
    while live > p {
        match PEAK.compare_exchange_weak(p, live, Ordering::Relaxed, Ordering::Relaxed) { Ok(_) => { peak_snapshot(live); break } Err(x) => p = x }
    }
}

static SNAP_AT: AtomicI64 = AtomicI64::new(0);
const SNAP_STEP: i64 = 16 << 20;
struct PeakSnap { live: i64, sites: HashMap<u64, (usize, usize)>, arena_end: usize, arena_free: usize, arena_largest: usize }
static PEAK_SNAP: Mutex<Option<PeakSnap>> = Mutex::new(None);

#[cold]
fn peak_snapshot(live: i64) {
    if !TRACKING.load(Ordering::Relaxed) || live < SNAP_AT.load(Ordering::Relaxed) + SNAP_STEP { return; }
    let _ = IN_TRACKER.try_with(|g| {
        if g.replace(true) { return; }
        let mut sites: HashMap<u64, (usize, usize)> = HashMap::new();
        let ok = if let Ok(t) = TRACKED.try_lock() {
            if let Some((live_map, _)) = t.as_ref() { for (_, (size, id)) in live_map.iter() { let e = sites.entry(*id).or_default(); e.0 += size; e.1 += 1; } }
            true
        } else { false };
        if ok {
            let (arena_end, arena_free, arena_largest) = { let _h = heap_lock(); let a = arena(); (WasmGrow.end() << SHIFT, a.free_units() << SHIFT, a.largest_free() << SHIFT) };
            if let Ok(mut s) = PEAK_SNAP.try_lock() { *s = Some(PeakSnap { live, sites, arena_end, arena_free, arena_largest }); SNAP_AT.store(live, Ordering::Relaxed); }
        }
        g.set(false);
    });
}

#[inline]
fn account(bytes: i64, allocs: u32) {
    let ok = DELTA.try_with(|d| {
        let v = d.get() + bytes;
        let n = NALLOC.try_with(|c| { let k = c.get() + allocs; c.set(k); k }).unwrap_or(0);
        if v.abs() >= FLUSH_BYTES || n >= FLUSH_ALLOCS {
            d.set(0);
            let _ = NALLOC.try_with(|c| c.set(0));
            flush(v, n);
        } else { d.set(v); }
    });
    if ok.is_err() { flush(bytes, allocs); }
}
#[inline]
fn add(n: usize) { account(n as i64, 0); }
#[inline]
fn sub(n: usize) { account(-(n as i64), 0); }

static HEAP_LOCK: AtomicI32 = AtomicI32::new(0);
pub static MAIN_LOCK_WAIT_US: AtomicI64 = AtomicI64::new(0);
static MAIN_WANTS: AtomicI32 = AtomicI32::new(0);
pub static GROWS: AtomicUsize = AtomicUsize::new(0);
pub static GROW_US: AtomicI64 = AtomicI64::new(0);
pub static GROW_MAX_US: AtomicI64 = AtomicI64::new(0);
pub static GROW_MAIN: AtomicUsize = AtomicUsize::new(0);
pub static GROW_WANT_KB: AtomicUsize = AtomicUsize::new(0);
pub static GROW_FREE_KB: AtomicUsize = AtomicUsize::new(0);
pub static GROW_LARGEST_KB: AtomicUsize = AtomicUsize::new(0);
struct HeapGuard;
pub static HOLDER: AtomicI32 = AtomicI32::new(0);
static DUMP_REQ: AtomicI32 = AtomicI32::new(0);
thread_local! { static IN_DUMP: Cell<bool> = const { Cell::new(false) }; }
#[inline]
fn mark_holder() { HOLDER.store(crate::pool::worker_index() + 2, Ordering::Relaxed); }
#[inline]
fn heap_lock() -> HeapGuard {
    if MAIN_WANTS.load(Ordering::Relaxed) == 0 && HEAP_LOCK.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() { return locked(); }
    let _ = heap_lock_slow(None);
    locked()
}
#[inline]
fn locked() -> HeapGuard {
    mark_holder();
    if DEFER.load(Ordering::Relaxed) != 0 { drain_deferred(); }
    HeapGuard
}
#[inline]
fn lock_or_fallback() -> Option<HeapGuard> {
    if MAIN_WANTS.load(Ordering::Relaxed) == 0 && HEAP_LOCK.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() { return Some(locked()); }
    if on_main() && MREG_END.load(Ordering::Relaxed) != 0 {
        let stuck = MAIN_STUCK.load(Ordering::Relaxed);
        if stuck != 0 && HEAP_LOCK.load(Ordering::Relaxed) != 0 && HOLDER.load(Ordering::Relaxed) == stuck { return None; }
        let r = heap_lock_slow(Some(MAIN_SPIN_US)).map(|_| locked());
        MAIN_STUCK.store(if r.is_none() { HOLDER.load(Ordering::Relaxed) } else { 0 }, Ordering::Relaxed);
        return r;
    }
    Some(heap_lock())
}
const MAIN_SPIN_US: u64 = 250;
static MAIN_STUCK: AtomicI32 = AtomicI32::new(0);
#[cold]
fn dump_holder() {
    if DUMP_REQ.swap(0, Ordering::Relaxed) == 0 { return; }
    let _ = IN_DUMP.try_with(|g| {
        if g.replace(true) { return; }
        let st = crate::js::fnweb_stack_deep(40);
        eprintln!("[heaplock holder] worker {} released after main waited: {st}", crate::pool::worker_index());
        g.set(false);
    });
}
#[cold]
fn heap_lock_slow(budget_us: Option<u64>) -> Option<()> {
    if !crate::pool::can_block() {
        if HEAP_LOCK.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() { return Some(()); }
        let t = std::time::Instant::now();
        MAIN_WANTS.store(1, Ordering::SeqCst);
        let (mut n, mut asked, mut changes, mut last) = (0u32, false, 0u32, HOLDER.load(Ordering::Relaxed));
        loop {
            if HEAP_LOCK.load(Ordering::Relaxed) == 0 && HEAP_LOCK.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                MAIN_WANTS.store(0, Ordering::Release); unsafe { core::arch::wasm32::memory_atomic_notify(MAIN_WANTS.as_ptr(), u32::MAX); }
                let us = t.elapsed().as_micros() as i64;
                MAIN_LOCK_WAIT_US.fetch_add(us, Ordering::Relaxed);
                if asked { LOCK_REPORT.store(((us / 1000) << 32) | ((changes as i64) << 16) | (last as i64 & 0xffff), Ordering::Relaxed); }
                return Some(());
            }
            n += 1;
            if let Some(b) = budget_us { if n & 63 == 0 && t.elapsed().as_micros() as u64 > b {
                MAIN_WANTS.store(0, Ordering::Release); unsafe { core::arch::wasm32::memory_atomic_notify(MAIN_WANTS.as_ptr(), u32::MAX); }
                MAIN_LOCK_WAIT_US.fetch_add(t.elapsed().as_micros() as i64, Ordering::Relaxed);
                FALLBACK_SPINS.fetch_add(1, Ordering::Relaxed);
                return None;
            } }
            if n & 1023 == 0 {
                let h = HOLDER.load(Ordering::Relaxed);
                if h != last { changes += 1; last = h; }
                if !asked && t.elapsed().as_micros() > 50_000 { asked = true; DUMP_REQ.store(1, Ordering::Relaxed); }
            }
            std::hint::spin_loop();
        }
    }
    wait_main_done();
    for _ in 0..32 {
        if MAIN_WANTS.load(Ordering::Relaxed) == 0 && HEAP_LOCK.load(Ordering::Relaxed) == 0 && HEAP_LOCK.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() { return Some(()); }
        std::hint::spin_loop();
    }
    let mut c = HEAP_LOCK.swap(2, Ordering::Acquire);
    while c != 0 {
        unsafe { core::arch::wasm32::memory_atomic_wait32(HEAP_LOCK.as_ptr(), 2, -1); }
        wait_main_done();
        c = HEAP_LOCK.swap(2, Ordering::Acquire);
    }
    Some(())
}

thread_local! { static IS_MAIN: Cell<bool> = const { Cell::new(false) }; static MAY_GROW: Cell<bool> = const { Cell::new(false) }; }
pub static GROW_DENIED: AtomicUsize = AtomicUsize::new(0);
static DENY_MAIN_GROW: AtomicBool = AtomicBool::new(false);
pub fn deny_main_grow() { DENY_MAIN_GROW.store(true, Ordering::Relaxed); }
#[cold]
fn main_retry(f: impl Fn() -> *mut u8, size: usize, align: usize, zero: bool) -> *mut u8 {
    inbox_drain();
    let p = unsafe { if zero { mdl().calloc(size, align) } else { mdl().malloc(size, align) } };
    if !p.is_null() { FALLBACK_ALLOCS.fetch_add(1, Ordering::Relaxed); return p; }
    let _ = MAY_GROW.try_with(|c| c.set(true));
    let q = f();
    let _ = MAY_GROW.try_with(|c| c.set(false));
    q
}
#[inline]
fn on_main() -> bool { IS_MAIN.try_with(|c| c.get()).unwrap_or(false) }
static MREG_START: AtomicUsize = AtomicUsize::new(0);
static MREG_END: AtomicUsize = AtomicUsize::new(0);
static MBUMP: AtomicUsize = AtomicUsize::new(0);
static INBOX: AtomicUsize = AtomicUsize::new(0);
static DEFER: AtomicUsize = AtomicUsize::new(0);
pub static FALLBACK_SPINS: AtomicUsize = AtomicUsize::new(0);
pub static FALLBACK_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static FALLBACK_DEFERS: AtomicUsize = AtomicUsize::new(0);
#[inline]
fn in_main(p: *mut u8) -> bool { let a = p as usize; a >= MREG_START.load(Ordering::Relaxed) && a < MREG_END.load(Ordering::Relaxed) }

struct MainSeg;
unsafe impl dlmalloc::Allocator for MainSeg {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        let n = size.next_multiple_of(UNIT);
        let (b, e) = (MBUMP.load(Ordering::Relaxed), MREG_END.load(Ordering::Relaxed));
        if b == 0 || b + n > e { return (std::ptr::null_mut(), 0, 0); }
        MBUMP.store(b + n, Ordering::Relaxed);
        (b as *mut u8, n, 0)
    }
    fn remap(&self, _: *mut u8, _: usize, _: usize, _: bool) -> *mut u8 { std::ptr::null_mut() }
    fn free_part(&self, _: *mut u8, _: usize, _: usize) -> bool { false }
    fn free(&self, _: *mut u8, _: usize) -> bool { false }
    fn can_release_part(&self, _: u32) -> bool { false }
    fn allocates_zeros(&self) -> bool { false }
    fn page_size(&self) -> usize { UNIT }
}
static mut MDL: dlmalloc::Dlmalloc<MainSeg> = { let mut d = dlmalloc::Dlmalloc::new_with_allocator(MainSeg); d.set_granularity(1 << 20); d };
#[inline]
fn mdl() -> &'static mut dlmalloc::Dlmalloc<MainSeg> { unsafe { &mut *core::ptr::addr_of_mut!(MDL) } }

pub fn main_heap_init(bytes: usize) {
    if crate::pool::can_block() || MREG_END.load(Ordering::Relaxed) != 0 { return; }
    let n = pages::units(bytes);
    let s = { let _g = heap_lock(); unsafe { arena().alloc(n, &mut WasmGrow) } };
    let Some(s) = s else { return };
    let (a, e) = (s << SHIFT, (s + n) << SHIFT);
    let mut q = a;
    while q < e { unsafe { std::ptr::write_volatile(q as *mut u8, 0) }; q += 4096; }
    MBUMP.store(a, Ordering::Relaxed);
    MREG_START.store(a, Ordering::Relaxed);
    MREG_END.store(e, Ordering::Release);
    let _ = IS_MAIN.try_with(|c| c.set(true));
}
pub fn main_heap_stats() -> (usize, usize, usize, usize) {
    (MBUMP.load(Ordering::Relaxed).saturating_sub(MREG_START.load(Ordering::Relaxed)), FALLBACK_SPINS.load(Ordering::Relaxed), FALLBACK_ALLOCS.load(Ordering::Relaxed), FALLBACK_DEFERS.load(Ordering::Relaxed))
}

#[inline]
fn free_main_block(p: *mut u8) {
    if on_main() { inbox_drain(); unsafe { mdl().c_free(p) }; } else { inbox_push(p); }
}
fn inbox_push(p: *mut u8) {
    loop {
        let h = INBOX.load(Ordering::Relaxed);
        unsafe { *(p as *mut usize) = h; }
        if INBOX.compare_exchange_weak(h, p as usize, Ordering::Release, Ordering::Relaxed).is_ok() { return; }
    }
}
#[inline]
fn inbox_drain() {
    if INBOX.load(Ordering::Relaxed) == 0 { return; }
    let mut h = INBOX.swap(0, Ordering::Acquire);
    while h != 0 { let n = unsafe { *(h as *const usize) }; unsafe { mdl().c_free(h as *mut u8) }; h = n; }
}
fn defer_push(p: *mut u8, word: usize) {
    FALLBACK_DEFERS.fetch_add(1, Ordering::Relaxed);
    unsafe { *(p as *mut usize).add(1) = word; }
    loop {
        let h = DEFER.load(Ordering::Relaxed);
        unsafe { *(p as *mut usize) = h; }
        if DEFER.compare_exchange_weak(h, p as usize, Ordering::Release, Ordering::Relaxed).is_ok() { return; }
    }
}
#[cold]
fn drain_deferred() {
    let mut h = DEFER.swap(0, Ordering::Acquire);
    while h != 0 {
        let (n, w) = unsafe { (*(h as *const usize), *(h as *const usize).add(1)) };
        if w & 1 == 1 { MED_BYTES.fetch_sub(w >> 1, Ordering::Relaxed); unsafe { dl().c_free(h as *mut u8) }; } else { big_sub(w >> 1); unsafe { arena().free(h >> SHIFT, w >> 1) }; }
        h = n;
    }
}
#[cold]
unsafe fn main_alloc(size: usize, align: usize, zero: bool) -> *mut u8 {
    inbox_drain();
    let p = unsafe { if zero { mdl().calloc(size, align) } else { mdl().malloc(size, align) } };
    if !p.is_null() { FALLBACK_ALLOCS.fetch_add(1, Ordering::Relaxed); return p; }
    let _g = heap_lock();
    unsafe { shared_alloc(Layout::from_size_align_unchecked(size, align), zero) }
}
#[inline]
fn wait_main_done() {
    let mut n = 0u32;
    while MAIN_WANTS.load(Ordering::Acquire) != 0 {
        if n < 64 { std::hint::spin_loop(); n += 1; } else { unsafe { core::arch::wasm32::memory_atomic_wait32(MAIN_WANTS.as_ptr(), 1, 200_000); } }
    }
}
pub static LOCK_REPORT: AtomicI64 = AtomicI64::new(0);
impl Drop for HeapGuard {
    #[inline]
    fn drop(&mut self) {
        HOLDER.store(0, Ordering::Relaxed);
        if HEAP_LOCK.swap(0, Ordering::Release) == 2 { unsafe { core::arch::wasm32::memory_atomic_notify(HEAP_LOCK.as_ptr(), 1); } }
        if DUMP_REQ.load(Ordering::Relaxed) != 0 && crate::pool::can_block() { dump_holder(); }
    }
}

#[path = "pages.rs"]
mod pages;
use pages::{Grow, Pages, SHIFT, UNIT};

const LARGE: usize = 256 << 10;
const NUNITS: usize = 1 << (32 - SHIFT);

const MIN_GROW_PAGES: usize = (16 << 20) >> 16;
struct WasmGrow;
impl Grow for WasmGrow {
    fn end(&self) -> usize { (core::arch::wasm32::memory_size(0) << 16) >> SHIFT }
    fn note(&mut self, want: usize, free: usize, largest: usize) {
        GROW_WANT_KB.store((want << SHIFT) >> 10, Ordering::Relaxed);
        GROW_FREE_KB.store((free << SHIFT) >> 10, Ordering::Relaxed);
        GROW_LARGEST_KB.store((largest << SHIFT) >> 10, Ordering::Relaxed);
        if !crate::pool::can_block() { GROW_MAIN.fetch_add(1, Ordering::Relaxed); }
    }
    fn grow(&mut self, units: usize) -> Option<usize> {
        if DENY_MAIN_GROW.load(Ordering::Relaxed) && on_main() && !MAY_GROW.try_with(|c| c.get()).unwrap_or(true) { GROW_DENIED.fetch_add(1, Ordering::Relaxed); return None; }
        let pages = (units << SHIFT).div_ceil(65536);
        let t = std::time::Instant::now();
        let want = pages.max(MIN_GROW_PAGES);
        let mut got = want;
        let mut r = core::arch::wasm32::memory_grow(0, want);
        if r == usize::MAX && want > pages { got = pages; r = core::arch::wasm32::memory_grow(0, pages); }
        GROWS.fetch_add(1, Ordering::Relaxed);
        let us = t.elapsed().as_micros() as i64;
        GROW_US.fetch_add(us, Ordering::Relaxed);
        GROW_MAX_US.fetch_max(us, Ordering::Relaxed);
        if r == usize::MAX { return None; }
        Some((got << 16) >> SHIFT)
    }
}

static mut ARENA: Pages<NUNITS> = Pages::new(0);
static ADOPTED: AtomicBool = AtomicBool::new(false);
#[inline]
fn arena() -> &'static mut Pages<NUNITS> {
    let a = unsafe { &mut *core::ptr::addr_of_mut!(ARENA) };
    if !ADOPTED.load(Ordering::Relaxed) { adopt_initial(a); }
    a
}
#[cold]
fn adopt_initial(a: &mut Pages<NUNITS>) {
    ADOPTED.store(true, Ordering::Relaxed);
    extern "C" { static __heap_base: u8; }
    let hb = core::ptr::addr_of!(__heap_base) as usize;
    let start = (hb.next_multiple_of(65536) + (1 << 20)).div_ceil(UNIT);
    let end = WasmGrow.end();
    if end > start && end < NUNITS { unsafe { a.free(start, end - start) }; }
}

pub static SEG_UNITS: AtomicUsize = AtomicUsize::new(0);
pub static BIG_UNITS: AtomicUsize = AtomicUsize::new(0);
pub static CLS_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static MED_BYTES: AtomicUsize = AtomicUsize::new(0);
#[inline]
fn big_add(n: usize) { BIG_UNITS.fetch_add(n, Ordering::Relaxed); }
#[inline]
fn big_sub(n: usize) { BIG_UNITS.fetch_sub(n, Ordering::Relaxed); }
struct Seg;
unsafe impl dlmalloc::Allocator for Seg {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        let n = pages::units(size);
        match unsafe { arena().alloc_low(n, &mut WasmGrow) } { Some(s) => { SEG_UNITS.fetch_add(n, Ordering::Relaxed); ((s << SHIFT) as *mut u8, n << SHIFT, 0) } None => (std::ptr::null_mut(), 0, 0) }
    }
    fn remap(&self, _: *mut u8, _: usize, _: usize, _: bool) -> *mut u8 { std::ptr::null_mut() }
    fn free_part(&self, ptr: *mut u8, old: usize, new: usize) -> bool {
        if (ptr as usize | new | old) & (UNIT - 1) != 0 || new >= old { return false; }
        unsafe { arena().shrink(ptr as usize >> SHIFT, old >> SHIFT, new >> SHIFT) };
        SEG_UNITS.fetch_sub((old - new) >> SHIFT, Ordering::Relaxed);
        true
    }
    fn free(&self, ptr: *mut u8, size: usize) -> bool {
        if (ptr as usize | size) & (UNIT - 1) != 0 { return false; }
        unsafe { arena().free(ptr as usize >> SHIFT, size >> SHIFT) };
        SEG_UNITS.fetch_sub(size >> SHIFT, Ordering::Relaxed);
        true
    }
    fn can_release_part(&self, _: u32) -> bool { true }
    fn allocates_zeros(&self) -> bool { false }
    fn page_size(&self) -> usize { UNIT }
}

static mut DL: dlmalloc::Dlmalloc<Seg> = { let mut d = dlmalloc::Dlmalloc::new_with_allocator(Seg); d.set_granularity(1 << 20); d };
#[inline]
fn dl() -> &'static mut dlmalloc::Dlmalloc<Seg> { unsafe { &mut *core::ptr::addr_of_mut!(DL) } }

#[inline]
fn big(size: usize, align: usize) -> bool { size >= LARGE && align <= UNIT }

unsafe fn shared_alloc(l: Layout, zero: bool) -> *mut u8 {
    if big(l.size(), l.align()) {
        let n = pages::units(l.size());
        match unsafe { arena().alloc(n, &mut WasmGrow) } { Some(s) => { big_add(n); (s << SHIFT) as *mut u8 } None => std::ptr::null_mut() }
    } else {
        let p = if zero { unsafe { dl().calloc(l.size(), l.align()) } } else { unsafe { dl().malloc(l.size(), l.align()) } };
        if !p.is_null() { MED_BYTES.fetch_add(l.size(), Ordering::Relaxed); }
        p
    }
}
unsafe fn raw_alloc(l: Layout) -> *mut u8 {
    match lock_or_fallback() {
        Some(_g) => unsafe { shared_alloc(l, false) },
        None => unsafe { main_alloc(l.size(), l.align(), false) },
    }
}
unsafe fn raw_alloc_zeroed(l: Layout) -> *mut u8 {
    if big(l.size(), l.align()) {
        let p = unsafe { raw_alloc(l) };
        if !p.is_null() { unsafe { std::ptr::write_bytes(p, 0, l.size()) }; }
        return p;
    }
    match lock_or_fallback() {
        Some(_g) => { let p = unsafe { dl().calloc(l.size(), l.align()) }; if !p.is_null() { MED_BYTES.fetch_add(l.size(), Ordering::Relaxed); } p }
        None => unsafe { main_alloc(l.size(), l.align(), true) },
    }
}
unsafe fn raw_free(p: *mut u8, l: Layout) {
    if in_main(p) { free_main_block(p); return; }
    let b = big(l.size(), l.align());
    match lock_or_fallback() {
        Some(_g) => if b { big_sub(pages::units(l.size())); unsafe { arena().free(p as usize >> SHIFT, pages::units(l.size())) } } else { MED_BYTES.fetch_sub(l.size(), Ordering::Relaxed); unsafe { dl().free(p, l.size(), l.align()) } },
        None => defer_push(p, if b { pages::units(l.size()) << 1 } else { (l.size() << 1) | 1 }),
    }
}
unsafe fn raw_realloc(p: *mut u8, l: Layout, new: usize) -> *mut u8 {
    if in_main(p) {
        if on_main() { inbox_drain(); let q = unsafe { mdl().realloc(p, l.size(), l.align(), new) }; if !q.is_null() { return q; } }
        let q = unsafe { raw_alloc(Layout::from_size_align_unchecked(new, l.align())) };
        if !q.is_null() { unsafe { std::ptr::copy_nonoverlapping(p, q, l.size().min(new)); raw_free(p, l); } }
        return q;
    }
    let (ob, nb) = (big(l.size(), l.align()), big(new, l.align()));
    if ob && nb {
        let (s, n, n2) = (p as usize >> SHIFT, pages::units(l.size()), pages::units(new));
        if let Some(_g) = lock_or_fallback() {
            if n2 <= n { big_sub(n - n2); unsafe { arena().shrink(s, n, n2) }; return p; }
            if unsafe { arena().grow_in_place(s, n, n2, &mut WasmGrow) } { big_add(n2 - n); return p; }
        }
        let q = unsafe { raw_alloc(Layout::from_size_align_unchecked(new, l.align())) };
        if !q.is_null() { unsafe { std::ptr::copy_nonoverlapping(p, q, l.size().min(new)); raw_free(p, l); } }
        return q;
    }
    if !ob && !nb { if let Some(_g) = lock_or_fallback() { let q = unsafe { dl().realloc(p, l.size(), l.align(), new) }; if !q.is_null() { MED_BYTES.fetch_add(new, Ordering::Relaxed); MED_BYTES.fetch_sub(l.size(), Ordering::Relaxed); } return q; } }
    let q = unsafe { raw_alloc(Layout::from_size_align_unchecked(new, l.align())) };
    if !q.is_null() { unsafe { std::ptr::copy_nonoverlapping(p, q, l.size().min(new)); raw_free(p, l); } }
    q
}

pub fn reserve_step(target_bytes: usize, step_bytes: usize) -> bool {
    let cur = (core::arch::wasm32::memory_size(0) as u64) << 16;
    if cur >= target_bytes as u64 { return false; }
    let pages = (step_bytes as u64).min(target_bytes as u64 - cur).div_ceil(65536) as usize;
    let _g = heap_lock();
    let a = arena();
    let t = std::time::Instant::now();
    let r = core::arch::wasm32::memory_grow(0, pages);
    let us = t.elapsed().as_micros() as i64;
    RESERVE_US.fetch_add(us, Ordering::Relaxed);
    if r == usize::MAX { return false; }
    let (s, n) = ((r << 16) >> SHIFT, (pages << 16) >> SHIFT);
    if s + n <= NUNITS { unsafe { a.free(s, n) }; }
    true
}
pub static RESERVE_US: AtomicI64 = AtomicI64::new(0);
pub static HEADROOM_GROWS: AtomicUsize = AtomicUsize::new(0);
pub static TRIMS: AtomicUsize = AtomicUsize::new(0);
pub fn ensure_headroom(min_largest: usize, step_bytes: usize) -> bool {
    let _g = heap_lock();
    let a = arena();
    if a.largest_free() << SHIFT >= min_largest { return false; }
    if unsafe { dl().trim(0) } { TRIMS.fetch_add(1, Ordering::Relaxed); if arena().largest_free() << SHIFT >= min_largest { return false; } }
    let a = arena();
    if step_bytes == 0 { return false; }
    let pages = step_bytes.div_ceil(65536);
    let r = core::arch::wasm32::memory_grow(0, pages);
    if r == usize::MAX { return false; }
    let (s, n) = ((r << 16) >> SHIFT, (pages << 16) >> SHIFT);
    if s + n <= NUNITS { unsafe { a.free(s, n) }; }
    HEADROOM_GROWS.fetch_add(1, Ordering::Relaxed);
    true
}
pub fn arena_free() -> (usize, usize) { let _g = heap_lock(); let a = arena(); (a.free_units() << SHIFT, a.largest_free() << SHIFT) }

pub fn mem_map() -> String {
    let (end, free, largest) = { let _h = heap_lock(); let a = arena(); (WasmGrow.end() << SHIFT, a.free_units() << SHIFT, a.largest_free() << SHIFT) };
    let mb = |x: usize| x as f64 / 1048576.;
    let (seg, big, cls, med) = (SEG_UNITS.load(Ordering::Relaxed) << SHIFT, BIG_UNITS.load(Ordering::Relaxed) << SHIFT, CLS_BYTES.load(Ordering::Relaxed), MED_BYTES.load(Ordering::Relaxed));
    let main = MREG_END.load(Ordering::Relaxed).saturating_sub(MREG_START.load(Ordering::Relaxed));
    let rest = end.saturating_sub(free + seg + big + main);
    let (slab, sempty) = (SLAB_UNITS.load(Ordering::Relaxed) << SHIFT, SLAB_EMPTY_UNITS.load(Ordering::Relaxed) << SHIFT);
    let rest = rest.saturating_sub(slab);
    format!("[memmap] wasm {:.0} live {:.0} | arena free {:.0} (largest {:.0}) | big {:.0} | slabs {:.0} = cls {:.0} + empty {:.0} + partial {:.0} | dl seg {:.0} = med {:.0} + dl slack {:.0} | main {:.0} | base+other {:.0}", mb(end), mb(LIVE.load(Ordering::Relaxed).max(0) as usize), mb(free), mb(largest), mb(big), mb(slab), mb(cls), mb(sempty), mb(slab.saturating_sub(cls + sempty)), mb(seg), mb(med), mb(seg.saturating_sub(med)), mb(main), mb(rest))
}

pub fn trim() { let _g = heap_lock(); unsafe { dl().trim(0) }; }

const NCLS: usize = 28;
const CLASSES: [usize; NCLS] = [16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1280, 1536, 1792, 2048, 2560, 3072, 3584, 4096];
const fn cls_lut() -> [u8; 257] {
    let mut t = [0u8; 257];
    let (mut i, mut c) = (0usize, 0usize);
    while i < 257 { while CLASSES[c] < i * 16 { c += 1; } t[i] = c as u8; i += 1; }
    t
}
static CLS_LUT: [u8; 257] = cls_lut();
const fn per_class(budget: usize, lo: u32, hi: u32) -> [u32; NCLS] {
    let mut t = [0u32; NCLS];
    let mut c = 0;
    while c < NCLS { let v = (budget / CLASSES[c]) as u32; t[c] = if v < lo { lo } else if v > hi { hi } else { v }; c += 1; }
    t
}
const CAPS: [u32; NCLS] = per_class(8192, 4, 512);
const MAIN_CAPS: [u32; NCLS] = per_class(65536, 16, 4096);
const BATCH: [u32; NCLS] = per_class(1024, 1, 32);
#[inline]
fn class_of(l: Layout) -> Option<usize> {
    if l.align() > 16 || l.size() == 0 || l.size() > 4096 { return None; }
    Some(CLS_LUT[l.size().div_ceil(16)] as usize)
}
#[inline]
fn class_layout(c: usize) -> Layout { unsafe { Layout::from_size_align_unchecked(CLASSES[c], 16) } }

const CHUNK_UNITS: usize = 256;
const MAX_CHUNKS: usize = NUNITS / CHUNK_UNITS;
#[derive(Clone, Copy)]
struct SMeta { free: u32, next: u32, prev: u32, used: u16, bump: u16, cls: u8, kind: u8, chunk: u16 }
const SMETA0: SMeta = SMeta { free: 0, next: 0, prev: 0, used: 0, bump: 0, cls: 0, kind: 0, chunk: 0 };
#[derive(Clone, Copy)]
struct SChunk { base: u32, empty: u16, total: u16 }
struct Slabs { meta: [SMeta; NUNITS], partial: [u32; NCLS], empty: [u32; 2], empty_n: [usize; 2], chunks: [SChunk; MAX_CHUNKS] }
static mut SLABS: Slabs = Slabs { meta: [SMETA0; NUNITS], partial: [0; NCLS], empty: [0; 2], empty_n: [0; 2], chunks: [SChunk { base: 0, empty: 0, total: 0 }; MAX_CHUNKS] };
pub static SLAB_UNITS: AtomicUsize = AtomicUsize::new(0);
pub static SLAB_EMPTY_UNITS: AtomicUsize = AtomicUsize::new(0);
#[inline]
fn slabs() -> &'static mut Slabs { unsafe { &mut *core::ptr::addr_of_mut!(SLABS) } }
#[inline]
fn slab_units(c: usize) -> usize { if CLASSES[c] <= 2048 { 1 } else { 4 } }
#[inline]
fn slab_kind(su: usize) -> usize { if su == 1 { 0 } else { 1 } }
impl Slabs {
    fn push(meta: &mut [SMeta; NUNITS], head: &mut u32, u: usize) {
        meta[u].prev = 0;
        meta[u].next = *head;
        if *head != 0 { meta[*head as usize - 1].prev = u as u32 + 1; }
        *head = u as u32 + 1;
    }
    fn remove(meta: &mut [SMeta; NUNITS], head: &mut u32, u: usize) {
        let (n, p) = (meta[u].next, meta[u].prev);
        if p != 0 { meta[p as usize - 1].next = n; } else { *head = n; }
        if n != 0 { meta[n as usize - 1].prev = p; }
        meta[u].next = 0;
        meta[u].prev = 0;
    }
    fn slab_of(&self, p: usize) -> Option<usize> {
        let u = p >> SHIFT;
        if u >= NUNITS { return None; }
        match self.meta[u].kind { 1 => Some(u), 2 => Some(self.meta[u].next as usize), _ => None }
    }
    unsafe fn chunk_new(&mut self, su: usize) -> bool {
        let k = slab_kind(su);
        let Some(id) = self.chunks.iter().position(|c| c.total == 0) else { return false };
        let Some(base) = (unsafe { arena().alloc_low(CHUNK_UNITS, &mut WasmGrow) }) else { return false };
        let total = CHUNK_UNITS / su;
        self.chunks[id] = SChunk { base: base as u32, empty: total as u16, total: total as u16 };
        for i in (0..total).rev() {
            let u = base + i * su;
            self.meta[u] = SMeta { kind: 1, chunk: id as u16, ..SMETA0 };
            for t in 1..su { self.meta[u + t] = SMeta { kind: 2, next: u as u32, ..SMETA0 }; }
            Self::push(&mut self.meta, &mut self.empty[k], u);
        }
        self.empty_n[k] += total;
        SLAB_UNITS.fetch_add(CHUNK_UNITS, Ordering::Relaxed);
        SLAB_EMPTY_UNITS.fetch_add(CHUNK_UNITS, Ordering::Relaxed);
        true
    }
    unsafe fn chunk_release(&mut self, id: usize, su: usize) {
        let k = slab_kind(su);
        let SChunk { base, total, .. } = self.chunks[id];
        let (base, total) = (base as usize, total as usize);
        for i in 0..total { Self::remove(&mut self.meta, &mut self.empty[k], base + i * su); }
        for u in base..base + CHUNK_UNITS { self.meta[u] = SMETA0; }
        self.empty_n[k] -= total;
        self.chunks[id] = SChunk { base: 0, empty: 0, total: 0 };
        unsafe { arena().free(base, CHUNK_UNITS) };
        SLAB_UNITS.fetch_sub(CHUNK_UNITS, Ordering::Relaxed);
        SLAB_EMPTY_UNITS.fetch_sub(CHUNK_UNITS, Ordering::Relaxed);
    }
    unsafe fn alloc(&mut self, c: usize) -> *mut u8 {
        let su = slab_units(c);
        let size = CLASSES[c];
        let u = if self.partial[c] != 0 { self.partial[c] as usize - 1 } else {
            let k = slab_kind(su);
            if self.empty[k] == 0 && !unsafe { self.chunk_new(su) } { return std::ptr::null_mut(); }
            let u = self.empty[k] as usize - 1;
            Self::remove(&mut self.meta, &mut self.empty[k], u);
            self.empty_n[k] -= 1;
            let id = self.meta[u].chunk as usize;
            self.chunks[id].empty -= 1;
            SLAB_EMPTY_UNITS.fetch_sub(su, Ordering::Relaxed);
            let m = &mut self.meta[u];
            m.cls = c as u8; m.used = 0; m.bump = 0; m.free = 0;
            Self::push(&mut self.meta, &mut self.partial[c], u);
            u
        };
        let cap = (su << SHIFT) / size;
        let m = &mut self.meta[u];
        let p = if m.free != 0 { let p = m.free as usize; m.free = unsafe { *(p as *const u32) }; p } else { let p = (u << SHIFT) + m.bump as usize * size; m.bump += 1; p };
        m.used += 1;
        if m.used as usize == cap { Self::remove(&mut self.meta, &mut self.partial[c], u); }
        CLS_BYTES.fetch_add(size, Ordering::Relaxed);
        p as *mut u8
    }
    unsafe fn free(&mut self, u: usize, p: *mut u8) {
        let c = self.meta[u].cls as usize;
        let su = slab_units(c);
        let cap = (su << SHIFT) / CLASSES[c];
        CLS_BYTES.fetch_sub(CLASSES[c], Ordering::Relaxed);
        let m = &mut self.meta[u];
        unsafe { *(p as *mut u32) = m.free; }
        m.free = p as u32;
        let was_full = m.used as usize == cap;
        m.used -= 1;
        let now_empty = m.used == 0;
        if was_full && !now_empty { Self::push(&mut self.meta, &mut self.partial[c], u); }
        if !now_empty { return; }
        if !was_full { Self::remove(&mut self.meta, &mut self.partial[c], u); }
        let k = slab_kind(su);
        let id = self.meta[u].chunk as usize;
        self.meta[u].free = 0;
        Self::push(&mut self.meta, &mut self.empty[k], u);
        self.empty_n[k] += 1;
        self.chunks[id].empty += 1;
        SLAB_EMPTY_UNITS.fetch_add(su, Ordering::Relaxed);
        let total = self.chunks[id].total as usize;
        if self.chunks[id].empty as usize == total && self.empty_n[k] >= 2 * total { unsafe { self.chunk_release(id, su) }; }
    }
}
#[inline]
unsafe fn slab_free_any(p: *mut u8, l: Layout) {
    let s = slabs();
    match s.slab_of(p as usize) { Some(u) => unsafe { s.free(u, p) }, None => { MED_BYTES.fetch_sub(l.size(), Ordering::Relaxed); unsafe { dl().free(p, l.size(), l.align()) } } }
}
#[cold]
unsafe fn main_small(c: usize) -> *mut u8 {
    let l = class_layout(c);
    inbox_drain();
    let p = unsafe { mdl().malloc(l.size(), l.align()) };
    if !p.is_null() { FALLBACK_ALLOCS.fetch_add(1, Ordering::Relaxed); return p; }
    let _g = heap_lock();
    unsafe { slabs().alloc(c) }
}
thread_local! {
    static FREE: [Cell<*mut u8>; NCLS] = const { [const { Cell::new(std::ptr::null_mut()) }; NCLS] };
    static COUNT: [Cell<u32>; NCLS] = const { [const { Cell::new(0) }; NCLS] };
    static CAP: Cell<*const [u32; NCLS]> = const { Cell::new(std::ptr::null()) };
}
#[inline]
fn caps() -> &'static [u32; NCLS] {
    CAP.try_with(|c| {
        let p = c.get();
        if !p.is_null() { return unsafe { &*p }; }
        let v: &'static [u32; NCLS] = if crate::pool::can_block() { &CAPS } else { &MAIN_CAPS };
        c.set(v);
        v
    }).unwrap_or(&CAPS)
}
#[inline]
unsafe fn small_alloc(c: usize) -> *mut u8 {
    let hit = FREE.try_with(|f| { let p = f[c].get(); if !p.is_null() { f[c].set(unsafe { *(p as *mut *mut u8) }); let _ = COUNT.try_with(|n| n[c].set(n[c].get() - 1)); } p }).unwrap_or(std::ptr::null_mut());
    if !hit.is_null() { return hit; }
    unsafe { small_refill(c) }
}
#[cold]
unsafe fn small_refill(c: usize) -> *mut u8 {
    let room = caps()[c].saturating_sub(COUNT.try_with(|n| n[c].get()).unwrap_or(u32::MAX));
    let extra = BATCH[c].saturating_sub(1).min(room);
    let Some(_g) = lock_or_fallback() else { return unsafe { main_small(c) } };
    let s = slabs();
    let first = unsafe { s.alloc(c) };
    if first.is_null() || extra == 0 { return first; }
    let _ = FREE.try_with(|f| COUNT.try_with(|n| {
        for _ in 0..extra {
            let p = unsafe { s.alloc(c) };
            if p.is_null() { break; }
            unsafe { *(p as *mut *mut u8) = f[c].get(); }
            f[c].set(p);
            n[c].set(n[c].get() + 1);
        }
    }));
    first
}
#[inline]
unsafe fn small_free(p: *mut u8, c: usize) {
    let cap = caps()[c];
    let kept = COUNT.try_with(|n| { if n[c].get() >= cap { return false; } FREE.try_with(|f| { unsafe { *(p as *mut *mut u8) = f[c].get(); } f[c].set(p); n[c].set(n[c].get() + 1); true }).unwrap_or(false) }).unwrap_or(false);
    if !kept { unsafe { small_spill(p, c, cap) } }
}
#[cold]
unsafe fn small_spill(p: *mut u8, c: usize, cap: u32) {
    let l = class_layout(c);
    let Some(_g) = lock_or_fallback() else {
        let _ = FREE.try_with(|f| COUNT.try_with(|n| { unsafe { *(p as *mut *mut u8) = f[c].get(); } f[c].set(p); n[c].set(n[c].get() + 1); }));
        return;
    };
    if in_main(p) { free_main_block(p); } else { unsafe { slab_free_any(p, l) }; }
    let _ = FREE.try_with(|f| COUNT.try_with(|n| {
        let keep = cap / 2;
        while n[c].get() > keep {
            let q = f[c].get();
            if q.is_null() { n[c].set(0); break; }
            f[c].set(unsafe { *(q as *mut *mut u8) });
            n[c].set(n[c].get() - 1);
            if in_main(q) { free_main_block(q); } else { unsafe { slab_free_any(q, l) }; }
        }
    }));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let mut p = match class_of(l) { Some(c) => unsafe { small_alloc(c) }, None => unsafe { raw_alloc(l) } };
        if p.is_null() && on_main() { p = main_retry(|| match class_of(l) { Some(c) => unsafe { small_alloc(c) }, None => unsafe { raw_alloc(l) } }, l.size(), l.align().max(16), false); }
        if !p.is_null() { account(l.size() as i64, 1); if watched(p, l.size()) { track(p, l.size()); } }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let mut p = match class_of(l) { Some(c) => { let p = unsafe { small_alloc(c) }; if !p.is_null() { unsafe { std::ptr::write_bytes(p, 0, l.size()) }; } p } None => unsafe { raw_alloc_zeroed(l) } };
        if p.is_null() && on_main() { p = main_retry(|| match class_of(l) { Some(c) => { let p = unsafe { small_alloc(c) }; if !p.is_null() { unsafe { std::ptr::write_bytes(p, 0, l.size()) }; } p } None => unsafe { raw_alloc_zeroed(l) } }, l.size(), l.align().max(16), true); }
        if !p.is_null() { account(l.size() as i64, 1); if watched(p, l.size()) { track(p, l.size()); } }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if watched(p, l.size()) { untrack(p); }
        match class_of(l) { Some(c) => unsafe { small_free(p, c) }, None => unsafe { raw_free(p, l) } }
        sub(l.size());
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let nl = unsafe { Layout::from_size_align_unchecked(new, l.align()) };
        let (oc, nc) = (class_of(l), class_of(nl));
        if oc.is_none() && nc.is_none() {
            if watched(p, l.size()) { untrack(p); }
            let mut q = unsafe { raw_realloc(p, l, new) };
            if q.is_null() && on_main() {
                q = unsafe { self.alloc(nl) };
                if !q.is_null() { unsafe { std::ptr::copy_nonoverlapping(p, q, l.size().min(new)); } unsafe { self.dealloc(p, l) }; return q; }
            }
            if !q.is_null() {
                if new >= l.size() { add(new - l.size()); } else { sub(l.size() - new); }
                if watched(q, new) { track(q, new); }
            } else if watched(p, l.size()) { track(p, l.size()); }
            return q;
        }
        if oc.is_some() && oc == nc {
            if new >= l.size() { add(new - l.size()); } else { sub(l.size() - new); }
            return p;
        }
        let q = unsafe { self.alloc(nl) };
        if !q.is_null() { unsafe { std::ptr::copy_nonoverlapping(p, q, l.size().min(new)); self.dealloc(p, l); } }
        q
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// (live heap bytes, peak live heap bytes, wasm linear memory bytes, allocations so far)
pub fn stats() -> (usize, usize, usize, usize) {
    (LIVE.load(Ordering::Relaxed).max(0) as usize, PEAK.load(Ordering::Relaxed).max(0) as usize, core::arch::wasm32::memory_size(0) * 65536, ALLOCS.load(Ordering::Relaxed))
}

/// Resets the peak to the current live size (e.g. after loading, to see the steady state).
pub fn reset_peak() { PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed); }

/// The live tracked allocations grouped by call site, biggest first: "MB  count  site".
pub fn report(top: usize) -> String {
    let rows: Vec<(usize, usize, String)> = {
        let _g = IN_TRACKER.try_with(|g| g.replace(true)).unwrap_or(true);
        let t = TRACKED.lock();
        let mut by: HashMap<u64, (usize, usize)> = HashMap::new();
        let mut out = vec![];
        if let Ok(t) = t {
            if let Some((live, sites)) = t.as_ref() {
                for (_, (size, id)) in live.iter() { let e = by.entry(*id).or_default(); e.0 += size; e.1 += 1; }
                for (id, (bytes, n)) in by { out.push((bytes, n, sites.get(&id).cloned().unwrap_or_default())); }
            }
        }
        let _ = IN_TRACKER.try_with(|g| g.set(false));
        out
    };
    let mut rows = rows;
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let total: usize = rows.iter().map(|r| r.0).sum();
    let mut s = format!("tracked live allocations (>= {} KiB exact, smaller ones sampled 1:64 and scaled): ~{:.0} MB in {} sites (heap live {:.0} MB)\n", TRACK_MIN >> 10, total as f64 / 1048576., rows.len(), LIVE.load(Ordering::Relaxed) as f64 / 1048576.);
    let (end, free, largest) = { let _h = heap_lock(); let a = arena(); (WasmGrow.end() << SHIFT, a.free_units() << SHIFT, a.largest_free() << SHIFT) };
    s.push_str(&format!("arena: end {:.0} MB, free {:.0} MB, largest free run {:.0} MB\n", end as f64 / 1048576., free as f64 / 1048576., largest as f64 / 1048576.));
    for (bytes, n, site) in rows.into_iter().take(top) { s.push_str(&format!("{:8.1} MB {:5}x  {site}\n", bytes as f64 / 1048576., n)); }
    s
}

pub fn report_peak(top: usize) -> String {
    let _g = IN_TRACKER.try_with(|g| g.replace(true)).unwrap_or(true);
    let mut s = String::new();
    if let (Ok(snap), Ok(t)) = (PEAK_SNAP.lock(), TRACKED.lock()) {
        if let (Some(p), Some((_, names))) = (snap.as_ref(), t.as_ref()) {
            let mut rows: Vec<(usize, usize, &str)> = p.sites.iter().map(|(id, (b, n))| (*b, *n, names.get(id).map_or("", |x| x.as_str()))).collect();
            rows.sort_by(|a, b| b.0.cmp(&a.0));
            let total: usize = rows.iter().map(|r| r.0).sum();
            s = format!("peak snapshot at heap live {:.0} MB: tracked ~{:.0} MB in {} sites; arena end {:.0} MB, free {:.0} MB, largest free run {:.0} MB\n", p.live as f64 / 1048576., total as f64 / 1048576., rows.len(), p.arena_end as f64 / 1048576., p.arena_free as f64 / 1048576., p.arena_largest as f64 / 1048576.);
            for (bytes, n, site) in rows.into_iter().take(top) { s.push_str(&format!("{:8.1} MB {:5}x  {site}\n", bytes as f64 / 1048576., n)); }
        }
    }
    let _ = IN_TRACKER.try_with(|g| g.set(false));
    if s.is_empty() { s.push_str("no peak snapshot\n"); }
    s
}
