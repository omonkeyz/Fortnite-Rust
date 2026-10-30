//! # fn-web-rt: the browser platform layer of the Fortnite 1.8 browser port
//!
//! The browser build compiles std from a patched copy of rust-src (`tools/web/std-patch/fnweb-std.patch`, applied by `tools/web/build.sh`).
//! The patch routes std's platform layer for `wasm32-unknown-unknown` + atomics to `extern "Rust"` hooks named `__fnweb_*`; this crate
//! defines them:
//!
//! * **files** (`std::fs`, `Path::exists`, `read_dir`, ...): a virtual file system over the local launcher's HTTP API (`fortnite-web.exe`,
//!   crate `fn-web-serve`): `/fn/paks` and `/fn/assets` are the user's own game files (read-only, ranged reads through a block cache),
//!   `/fn/user` is a writable per-user folder on disk (settings), `/fn/tmp` lives in memory. See [`vfs`].
//! * **threads** (`std::thread::spawn` / `scope`): jobs for a pool of Web Workers that the page starts before `main`; see [`pool`].
//! * **clocks**: `Instant` = `performance.timeOrigin + performance.now()` (one origin for all threads), `SystemTime` = `Date.now()`.
//! * **stdout / stderr**: line-buffered to the browser console (worker lines are forwarded to the main thread); see [`console`].
//! * **environment / args**: set by the page (URL parameters); see [`env`].
//! * **blocking**: the main thread can never `memory.atomic.wait`: std spins there instead (`__fnweb_can_block` is false on it).
//!
//! Everything is `#[cfg(target_arch = "wasm32")]`: natively this crate is empty.
#![cfg(target_arch = "wasm32")]
#![cfg_attr(target_arch = "wasm32", feature(stdarch_wasm_atomic_wait))]

pub mod console;
pub mod env;
mod hooks;
pub mod js;
pub mod mem;
pub mod pool;
pub mod vfs;
pub mod webcook_files;

/// True on the browser main thread (the Window), false on pool workers.
pub fn is_main_thread() -> bool { !pool::can_block() }

/// Work counters the game bumps for the browser build's hitch log (fn-web quality_web.rs): POI kinds built on the main thread so far.
pub static POI_KINDS_BUILT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
