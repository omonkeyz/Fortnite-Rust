use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const MAGIC: &[u8; 4] = b"WCP1";
pub const INDEX_FILE: &str = "index.bin";
pub const WEB_BASE: &str = "/fn/assets/webcook/v1";
pub fn web_base() -> String { fn_live::var("FNWEB_WEBCOOK_DIR").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| WEB_BASE.to_owned()) }
pub fn pak_base() -> String { if fn_live::var("FNWEB_WEBCOOK_V2").is_ok_and(|v| v == "1") && fn_live::var("FNWEB_WEBCOOK_DIR").map_or(true, |v| v.is_empty()) { "/fn/assets/webcook/v2".to_owned() } else { web_base() } }

#[derive(Clone, Debug, Default)]
pub struct Chunk { pub file: u16, pub off: u64, pub clen: u32, pub raw: u32 }

#[derive(Clone, Debug, Default)]
pub struct PakIdx { pub name: String, pub len: u64, pub file: u16, pub off: u64, pub clen: u32, pub tail: u32, pub index: u32 }

#[derive(Clone, Debug, Default)]
pub struct EntryLoc { pub chunk: u32, pub at: u32, pub size: u32 }

#[derive(Default)]
pub struct Index {
    pub dir: PathBuf,
    pub files: Vec<String>,
    pub paks: Vec<PakIdx>,
    pub chunks: Vec<Chunk>,
    pub entries: HashMap<(u16, u64), EntryLoc>,
    pub pak_ids: HashMap<String, u16>,
}

struct W(Vec<u8>);
impl W {
    fn u16(&mut self, v: u16) { self.0.extend_from_slice(&v.to_le_bytes()) }
    fn u32(&mut self, v: u32) { self.0.extend_from_slice(&v.to_le_bytes()) }
    fn u64(&mut self, v: u64) { self.0.extend_from_slice(&v.to_le_bytes()) }
    fn s(&mut self, v: &str) { self.u16(v.len() as u16); self.0.extend_from_slice(v.as_bytes()) }
}

pub struct Rd<'a> { pub b: &'a [u8], pub p: usize }
impl<'a> Rd<'a> {
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], String> { let s = self.b.get(self.p..self.p + n).ok_or("webcook index truncated")?; self.p += n; Ok(s) }
    pub fn u8(&mut self) -> Result<u8, String> { Ok(self.take(1)?[0]) }
    pub fn u16(&mut self) -> Result<u16, String> { Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap())) }
    pub fn u32(&mut self) -> Result<u32, String> { Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap())) }
    pub fn u64(&mut self) -> Result<u64, String> { Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap())) }
    pub fn s(&mut self) -> Result<String, String> { let n = self.u16()? as usize; Ok(String::from_utf8_lossy(self.take(n)?).into_owned()) }
}

pub fn unzstd(input: &[u8], size: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(size);
    ruzstd::decoding::FrameDecoder::new().decode_all_to_vec(input, &mut out).map_err(|e| format!("zstd: {e:?}"))?;
    if out.len() != size { return Err(format!("zstd: {} bytes, expected {size}", out.len())); }
    Ok(out)
}

impl Index {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = W(Vec::new());
        w.0.extend_from_slice(MAGIC);
        w.u32(self.files.len() as u32);
        for f in &self.files { w.s(f); }
        w.u32(self.paks.len() as u32);
        for p in &self.paks { w.s(&p.name); w.u64(p.len); w.u16(p.file); w.u64(p.off); w.u32(p.clen); w.u32(p.tail); w.u32(p.index); }
        w.u32(self.chunks.len() as u32);
        for c in &self.chunks { w.u16(c.file); w.u64(c.off); w.u32(c.clen); w.u32(c.raw); }
        let mut es: Vec<(&(u16, u64), &EntryLoc)> = self.entries.iter().collect();
        es.sort_by_key(|(k, _)| **k);
        w.u32(es.len() as u32);
        for ((pak, off), l) in es { w.u16(*pak); w.u64(*off); w.u32(l.chunk); w.u32(l.at); w.u32(l.size); }
        w.0
    }

    pub fn from_bytes(dir: &Path, b: &[u8]) -> Result<Index, String> {
        let mut r = Rd { b, p: 0 };
        if r.take(4)? != MAGIC { return Err("webcook pak index: bad magic".into()); }
        let mut ix = Index { dir: dir.to_owned(), ..Default::default() };
        for _ in 0..r.u32()? { ix.files.push(r.s()?); }
        for i in 0..r.u32()? {
            let p = PakIdx { name: r.s()?, len: r.u64()?, file: r.u16()?, off: r.u64()?, clen: r.u32()?, tail: r.u32()?, index: r.u32()? };
            ix.pak_ids.insert(p.name.to_ascii_lowercase(), i as u16);
            ix.paks.push(p);
        }
        let n = r.u32()? as usize;
        ix.chunks.reserve(n);
        for _ in 0..n { ix.chunks.push(Chunk { file: r.u16()?, off: r.u64()?, clen: r.u32()?, raw: r.u32()? }); }
        let n = r.u32()? as usize;
        ix.entries.reserve(n);
        for _ in 0..n { let k = (r.u16()?, r.u64()?); ix.entries.insert(k, EntryLoc { chunk: r.u32()?, at: r.u32()?, size: r.u32()? }); }
        Ok(ix)
    }

    pub fn open(dir: &Path) -> Result<Index, String> {
        let p = dir.join(INDEX_FILE);
        let z = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        if z.len() < 8 { return Err(format!("{}: too short", p.display())); }
        let raw = u32::from_le_bytes(z[..4].try_into().unwrap()) as usize;
        let b = unzstd(&z[4..], raw)?;
        Index::from_bytes(dir, &b)
    }

    fn read_file(&self, file: u16, off: u64, len: usize) -> Result<Vec<u8>, String> {
        let path = self.dir.join(self.files.get(file as usize).ok_or("webcook: bad file index")?);
        thread_local! { static FILES: std::cell::RefCell<HashMap<PathBuf, std::fs::File>> = std::cell::RefCell::new(HashMap::new()); }
        FILES.with(|f| {
            let mut f = f.borrow_mut();
            if !f.contains_key(&path) { let h = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?; f.insert(path.clone(), h); }
            let h = f.get_mut(&path).unwrap();
            let mut v = vec![0u8; len];
            h.seek(SeekFrom::Start(off)).and_then(|_| h.read_exact(&mut v)).map_err(|e| format!("{} @{off}+{len}: {e}", path.display()))?;
            Ok(v)
        })
    }

    pub fn chunk(&self, i: u32) -> Result<Arc<Vec<u8>>, String> {
        if let Some(c) = cache_get(i) { return Ok(c); }
        let c = self.chunks.get(i as usize).ok_or("webcook: bad chunk")?;
        if !AHEAD_THREAD.with(|a| a.get()) { decode_ahead(self, i); }
        loop {
            let mine = INFLIGHT.lock().map(|mut g| g.get_or_insert_with(Default::default).insert(i)).unwrap_or(true);
            if mine { break; }
            while INFLIGHT.lock().map(|g| g.as_ref().is_some_and(|h| h.contains(&i))).unwrap_or(false) { std::hint::spin_loop(); std::thread::yield_now(); }
            if let Some(d) = cache_get(i) { return Ok(d); }
        }
        let r = (|| {
            let z = self.read_file(c.file, c.off, c.clen as usize)?;
            let t0 = std::time::Instant::now();
            let d = Arc::new(unzstd(&z, c.raw as usize)?);
            note_decode(t0, c.raw as u64, c.clen as u64);
            cache_put(i, d.clone());
            Ok(d)
        })();
        if let Ok(mut g) = INFLIGHT.lock() { if let Some(h) = g.as_mut() { h.remove(&i); } }
        r
    }

    pub fn find(&self, pak: &str, entry_offset: u64) -> Option<&EntryLoc> {
        let id = *self.pak_ids.get(&pak.to_ascii_lowercase())?;
        self.entries.get(&(id, entry_offset))
    }

    pub fn read_entry(&self, l: &EntryLoc) -> Result<Vec<u8>, String> {
        let c = self.chunk(l.chunk)?;
        let s = c.get(l.at as usize..l.at as usize + l.size as usize).ok_or("webcook: entry outside its chunk")?;
        HITS.fetch_add(1, Ordering::Relaxed);
        HIT_BYTES.fetch_add(l.size as u64, Ordering::Relaxed);
        Ok(s.to_vec())
    }

    pub fn pak_index(&self, pak: &str) -> Option<Result<(u64, Vec<u8>, Vec<u8>), String>> {
        let id = *self.pak_ids.get(&pak.to_ascii_lowercase())?;
        let p = &self.paks[id as usize];
        if p.clen == 0 { return None; }
        Some((|| {
            let z = self.read_file(p.file, p.off, p.clen as usize)?;
            let mut b = unzstd(&z, (p.tail + p.index) as usize)?;
            let index = b.split_off(p.tail as usize);
            Ok((p.len, b, index))
        })())
    }
}

static HITS: AtomicU64 = AtomicU64::new(0);
static HIT_BYTES: AtomicU64 = AtomicU64::new(0);
static DECODES: AtomicU64 = AtomicU64::new(0);
static DECODE_US: AtomicU64 = AtomicU64::new(0);
static WIRE_BYTES: AtomicU64 = AtomicU64::new(0);
static MAIN_DECODES: AtomicU64 = AtomicU64::new(0);
static MAIN_DECODE_US: AtomicU64 = AtomicU64::new(0);
static MAIN_CHECK: OnceLock<fn() -> bool> = OnceLock::new();

pub fn set_main_thread_check(f: fn() -> bool) { let _ = MAIN_CHECK.set(f); }

fn note_decode(t0: std::time::Instant, _raw: u64, clen: u64) {
    let us = t0.elapsed().as_micros() as u64;
    DECODES.fetch_add(1, Ordering::Relaxed);
    DECODE_US.fetch_add(us, Ordering::Relaxed);
    WIRE_BYTES.fetch_add(clen, Ordering::Relaxed);
    if MAIN_CHECK.get().is_some_and(|f| f()) { MAIN_DECODES.fetch_add(1, Ordering::Relaxed); MAIN_DECODE_US.fetch_add(us, Ordering::Relaxed); }
}

static CACHE_BUDGET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(96 << 20);
pub fn set_cache_budget(bytes: usize) {
    CACHE_BUDGET.store(bytes, Ordering::Relaxed);
    if let Ok(mut g) = CACHE.lock() { if let Some(c) = g.as_mut() { while c.bytes > bytes { let Some(k) = c.order.pop_front() else { break }; if let Some(x) = c.map.remove(&k) { c.bytes -= x.len(); } } } }
}
const AHEAD_CHUNKS: u32 = 8;
const AHEAD_BYTES: u64 = 4 << 20;
const AHEAD_PREFIX_END: u32 = 3275;
static INFLIGHT: Mutex<Option<std::collections::HashSet<u32>>> = Mutex::new(None);
thread_local! { static AHEAD_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
static AHEAD_Q: Mutex<VecDeque<u32>> = Mutex::new(VecDeque::new());
static AHEAD_WAKE: std::sync::Condvar = std::sync::Condvar::new();
static AHEAD_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn ahead_off() -> bool { static OFF: OnceLock<bool> = OnceLock::new(); *OFF.get_or_init(|| fn_live::var("FNWEB_WEBCOOK_AHEAD").is_ok_and(|v| v == "0")) }
fn decode_ahead(ix: &Index, i: u32) {
    if ahead_off() || !cfg!(target_arch = "wasm32") { return; }
    if i + 1 >= AHEAD_PREFIX_END || ix.chunks.get(i as usize).is_none() { return; }
    {
        let Ok(mut q) = AHEAD_Q.lock() else { return };
        let mut bytes = 0u64;
        for k in i + 1..(i + 1 + AHEAD_CHUNKS).min(AHEAD_PREFIX_END) {
            let Some(c) = ix.chunks.get(k as usize) else { break };
            bytes += c.raw as u64;
            if bytes > AHEAD_BYTES { break; }
            if cache_get(k).is_some() || q.contains(&k) { continue; }
            q.push_back(k);
        }
        while q.len() > 256 { q.pop_front(); }
    }
    AHEAD_WAKE.notify_all();
    if !AHEAD_STARTED.swap(true, Ordering::Relaxed) {
        for n in 0..2 {
            let _ = std::thread::Builder::new().name(format!("webcook decode-ahead {n}")).spawn(|| {
                AHEAD_THREAD.with(|a| a.set(true));
                loop {
                    let k = {
                        let mut q = AHEAD_Q.lock().unwrap_or_else(|e| e.into_inner());
                        loop { if let Some(k) = q.pop_front() { break k; } q = AHEAD_WAKE.wait(q).unwrap_or_else(|e| e.into_inner()); }
                    };
                    if cache_get(k).is_some() { continue; }
                    if let Some(ix) = index() { let _ = ix.chunk(k); }
                }
            });
        }
    }
}
struct Lru { map: HashMap<u32, Arc<Vec<u8>>>, order: VecDeque<u32>, bytes: usize }
static CACHE: Mutex<Option<Lru>> = Mutex::new(None);
fn cache_get(i: u32) -> Option<Arc<Vec<u8>>> { CACHE.lock().ok()?.as_ref()?.map.get(&i).cloned() }
fn cache_put(i: u32, d: Arc<Vec<u8>>) {
    let budget = CACHE_BUDGET.load(Ordering::Relaxed);
    if d.len() > budget / 4 { return; }
    let Ok(mut g) = CACHE.lock() else { return };
    let c = g.get_or_insert_with(|| Lru { map: HashMap::new(), order: VecDeque::new(), bytes: 0 });
    if c.map.contains_key(&i) { return; }
    c.bytes += d.len();
    c.map.insert(i, d);
    c.order.push_back(i);
    while c.bytes > budget { let Some(k) = c.order.pop_front() else { break }; if let Some(x) = c.map.remove(&k) { c.bytes -= x.len(); } }
}

static INDEX: OnceLock<Option<Index>> = OnceLock::new();

fn configured_dir() -> Option<PathBuf> {
    if let Some(d) = fn_live::var_os("FN_WEBCOOK_PAK_DIR") { return Some(PathBuf::from(d)); }
    if cfg!(target_arch = "wasm32") && fn_live::var("FNWEB_WEBCOOK").map_or(true, |v| v != "0") { return Some(PathBuf::from(pak_base()).join("pak")); }
    None
}

pub fn index() -> Option<&'static Index> {
    INDEX.get_or_init(|| {
        let dir = configured_dir()?;
        if !dir.join(INDEX_FILE).exists() { return None; }
        let t0 = std::time::Instant::now();
        match Index::open(&dir) {
            Ok(ix) => { eprintln!("[webcook] pak pack: {} entries in {} chunks, {} pak indices, from {} ({:.0} ms)", ix.entries.len(), ix.chunks.len(), ix.paks.iter().filter(|p| p.clen > 0).count(), dir.display(), t0.elapsed().as_secs_f32() * 1000.); Some(ix) }
            Err(e) => { eprintln!("[webcook] pak pack off: {e}"); None }
        }
    }).as_ref()
}

pub fn stats() -> Option<String> {
    INDEX.get()?.as_ref()?;
    let ms = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1000.;
    Some(format!("webcook pak: {} entries ({:.1} MB) served, {} chunks decoded ({:.1} MB over the wire, {:.0} ms), main-thread decodes {} ({:.0} ms)",
        HITS.load(Ordering::Relaxed), HIT_BYTES.load(Ordering::Relaxed) as f64 / 1048576., DECODES.load(Ordering::Relaxed), WIRE_BYTES.load(Ordering::Relaxed) as f64 / 1048576., ms(&DECODE_US), MAIN_DECODES.load(Ordering::Relaxed), ms(&MAIN_DECODE_US)))
}
