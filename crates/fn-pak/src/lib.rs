//! # fn-pak
//!
//! Pure-Rust reader for Unreal Engine 4 `.pak` archives, as used by Fortnite 1.8 (UE 4.16, pak v3)
//! and Fortnite 1.10 (pak v4).
//!
//! * [`PakFile`]  - one memory-mapped archive: footer, mount point, full entry table, random access reads.
//! * [`PakSet`]   - every pak of an install mounted together with **case-insensitive** path lookup and
//!   *later pak overrides earlier pak* semantics (see [`pak_load_order`]).
//! * [`glob_match`] - the small glob dialect used by [`PakSet::glob`] (`*`, `**`, `?`, case-insensitive).
//!
//! Supported formats: footer versions 1..=8 with the classic *inline* index (the 1.8 install is v3 and
//! the 1.10 install is v4). v9+ (frozen index) and v10+ (path-hash/full-directory index, compact entries)
//! are refused with [`PakError::UnsupportedVersion`]. Entries may be stored (method 0) or zlib (method 1,
//! or `"Zlib"` for v8 name-based compression). AES-encrypted stored entries are decrypted (AES-256, UE4.16 `FAES::DecryptData`: ECB over the 16-aligned
//! data) when the key is given in the `FN_PAK_AES_KEY` environment variable (hex, optional 0x); the key is never stored in the repo. Without it they are reported.
//!
//! Paths: pak entries carry a mount point (`../../../`) plus a relative file name. All APIs that take a
//! "path" accept either the full form (`../../../FortniteGame/Content/Foo.uasset`), the engine-root-relative
//! form (`FortniteGame/Content/Foo.uasset`), backslashes, any case, or a long package path
//! (`/Game/Foo` -> see [`game_path_to_pak`]).

pub mod webcook;

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
use std::path::{Path, PathBuf};

#[cfg(not(target_arch = "wasm32"))]
use memmap2::Mmap;

/// Number of entry reads done by every [`PakFile::read`] of the process (a load counter: a screen that preloaded everything leaves it unchanged afterwards).
static READ_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Process-wide count of pak entry reads so far.
pub fn read_count() -> u64 { READ_COUNT.load(std::sync::atomic::Ordering::Relaxed) }

pub const PAK_MAGIC: u32 = 0x5A6F_12E1;

// ----------------------------------------------------------------------------------------------------
// errors
// ----------------------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum PakError {
    Io(std::io::Error),
    /// No pak footer magic found in the last 256 bytes.
    NoFooter(String),
    UnsupportedVersion(u32, String),
    EncryptedIndex(String),
    /// Index or entry data runs past the end of the file / buffer.
    Truncated(String),
    NotFound(String),
    Encrypted(String),
    UnsupportedCompression(String),
    Decompress(String),
    Other(String),
}

impl fmt::Display for PakError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PakError::Io(e) => write!(f, "io error: {e}"),
            PakError::NoFooter(p) => write!(f, "no pak footer in {p}"),
            PakError::UnsupportedVersion(v, p) => write!(f, "unsupported pak version {v} in {p}"),
            PakError::EncryptedIndex(p) => write!(f, "pak index is encrypted: {p}"),
            PakError::Truncated(w) => write!(f, "truncated data: {w}"),
            PakError::NotFound(p) => write!(f, "not found: {p}"),
            PakError::Encrypted(p) => write!(f, "entry is AES-encrypted: {p}"),
            PakError::UnsupportedCompression(m) => write!(f, "unsupported compression method {m}"),
            PakError::Decompress(m) => write!(f, "decompression failed: {m}"),
            PakError::Other(m) => write!(f, "{m}"),
        }
    }
}
impl std::error::Error for PakError {}
impl From<std::io::Error> for PakError {
    fn from(e: std::io::Error) -> Self {
        PakError::Io(e)
    }
}
pub type Result<T> = std::result::Result<T, PakError>;

// ----------------------------------------------------------------------------------------------------
// path helpers
// ----------------------------------------------------------------------------------------------------

/// Normalise a path to the lookup key: forward slashes, lower-case, no leading `../` / `./` / `/`.
pub fn normalize_path(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    loop {
        if let Some(r) = s.strip_prefix("../") {
            s = r.to_string();
        } else if let Some(r) = s.strip_prefix("./") {
            s = r.to_string();
        } else if let Some(r) = s.strip_prefix('/') {
            s = r.to_string();
        } else {
            break;
        }
    }
    s.to_lowercase()
}

/// Map a long package/object path to the pak path of its `.uasset`:
/// `/Game/Athena/Foo.Foo_C` -> `FortniteGame/Content/Athena/Foo.uasset`,
/// `/Engine/Foo` -> `Engine/Content/Foo.uasset`. Paths that are not `/Game` or `/Engine` return `None`.
/// `ext` is e.g. `"uasset"`, `"uexp"`.
pub fn game_path_to_pak(long: &str, ext: &str) -> Option<String> {
    let l = long.replace('\\', "/");
    let l = l.split('.').next().unwrap_or(&l); // drop `.ObjectName`
    let l = l.split(':').next().unwrap_or(l);
    if let Some(r) = l.strip_prefix("/Game/") {
        Some(format!("FortniteGame/Content/{r}.{ext}"))
    } else if let Some(r) = l.strip_prefix("/Engine/") {
        Some(format!("Engine/Content/{r}.{ext}"))
    } else {
        None
    }
}

/// Inverse of [`game_path_to_pak`] (extension dropped): `FortniteGame/Content/A/B.uasset` -> `/Game/A/B`.
pub fn pak_path_to_game(pak: &str) -> Option<String> {
    let n = pak.replace('\\', "/");
    let n = n.trim_start_matches("../").to_string();
    let stem = match n.rfind('.') {
        Some(i) if n[i..].find('/').is_none() => &n[..i],
        _ => &n[..],
    };
    let lower = stem.to_lowercase();
    if lower.starts_with("fortnitegame/content/") {
        Some(format!("/Game/{}", &stem["FortniteGame/Content/".len()..]))
    } else if lower.starts_with("engine/content/") {
        Some(format!("/Engine/{}", &stem["Engine/Content/".len()..]))
    } else {
        None
    }
}

/// Case-insensitive glob. `*` = any run of characters except `/`, `**` = any run including `/`,
/// `?` = one character except `/`. `pattern` and `text` are compared after [`normalize_path`] by callers.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    // PERF (browser port): ASCII paths (all of the 1.8 paks') match without allocating: the same recursion over bytes with an ASCII-lowercase
    // comparison, which is exactly `to_lowercase` for ASCII. `PakSet::glob` runs this for every one of the ~238k entries per call.
    if pattern.is_ascii() && text.is_ascii() {
        return glob_rec_ascii(pattern.as_bytes(), text.as_bytes());
    }
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    glob_rec(&p, &t)
}

/// [`glob_rec`] on ASCII bytes, case-insensitive.
fn glob_rec_ascii(p: &[u8], t: &[u8]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    if p[0] == b'*' {
        let mut k = 0;
        while k < p.len() && p[k] == b'*' {
            k += 1;
        }
        let cross = k >= 2;
        let rest = &p[k..];
        if cross && rest.first() == Some(&b'/') && glob_rec_ascii(&rest[1..], t) {
            return true;
        }
        for i in 0..=t.len() {
            if glob_rec_ascii(rest, &t[i..]) {
                return true;
            }
            if i < t.len() && t[i] == b'/' && !cross {
                break;
            }
        }
        return false;
    }
    if t.is_empty() {
        return false;
    }
    if p[0].to_ascii_lowercase() == t[0].to_ascii_lowercase() || (p[0] == b'?' && t[0] != b'/') {
        return glob_rec_ascii(&p[1..], &t[1..]);
    }
    false
}

fn glob_rec(p: &[char], t: &[char]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    if p[0] == '*' {
        let mut k = 0;
        while k < p.len() && p[k] == '*' {
            k += 1;
        }
        let cross = k >= 2;
        let rest = &p[k..];
        // `**/` also matches zero directories
        if cross && rest.first() == Some(&'/') && glob_rec(&rest[1..], t) {
            return true;
        }
        for i in 0..=t.len() {
            if glob_rec(rest, &t[i..]) {
                return true;
            }
            if i < t.len() && t[i] == '/' && !cross {
                break;
            }
        }
        return false;
    }
    if t.is_empty() {
        return false;
    }
    if p[0] == t[0] || (p[0] == '?' && t[0] != '/') {
        return glob_rec(&p[1..], &t[1..]);
    }
    false
}

// ----------------------------------------------------------------------------------------------------
// entry / footer
// ----------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct PakInfo {
    pub version: u32,
    pub index_offset: u64,
    pub index_size: u64,
    pub index_hash: [u8; 20],
    pub encrypted_index: bool,
    /// v8+: compression method names from the footer (index 1.. maps to these).
    pub compression_names: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PakEntry {
    /// Mount point + file name exactly as stored (`../../../FortniteGame/Content/...`).
    pub path: String,
    /// Offset of the in-pak entry header (the data follows the header).
    pub offset: u64,
    pub compressed_size: u64,
    pub size: u64,
    /// 0 = stored, 1 = zlib (v<8). For v8+ this is the compression-name index (1-based, 0 = stored).
    pub method: u32,
    pub hash: [u8; 20],
    /// (start, end) of each compressed block, **absolute** within the pak file.
    pub blocks: Vec<(u64, u64)>,
    pub encrypted: bool,
    pub block_size: u32,
    pub timestamp: Option<i64>,
}

impl PakEntry {
    /// Lookup key (see [`normalize_path`]).
    pub fn key(&self) -> String {
        normalize_path(&self.path)
    }
    /// Path relative to the engine root (mount point's `../../../` stripped).
    pub fn rel_path(&self) -> &str {
        self.path.trim_start_matches("../")
    }
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.p + n > self.b.len() {
            return Err(PakError::Truncated(format!("need {n} bytes at {}", self.p)));
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(self.u64()? as i64)
    }
    fn fstring(&mut self) -> Result<String> {
        let n = self.i32()?;
        if n == 0 {
            return Ok(String::new());
        }
        if n < 0 {
            let cnt = (-(n as i64)) as usize;
            let raw = self.take(cnt * 2)?;
            let u: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let end = if u.last() == Some(&0) { u.len() - 1 } else { u.len() };
            Ok(String::from_utf16_lossy(&u[..end]))
        } else {
            let raw = self.take(n as usize)?;
            let end = if raw.last() == Some(&0) { raw.len() - 1 } else { raw.len() };
            Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
        }
    }
}

/// Serialised size of the FPakEntry header that precedes entry data inside the pak.
fn entry_header_len(version: u32, method: u32, nblocks: usize) -> u64 {
    let mut n = 8 + 8 + 8; // offset(not in-data: see note) -- computed below
    // In-data header layout: Offset(8) Size(8) UncompressedSize(8) [Timestamp(8) if v1] Method(4) Hash(20)
    // [BlockCount(4) + 16*n if v>=3 && method!=0] [Flags(1) + BlockSize(4) if v>=3]
    n += if version < 2 { 8 } else { 0 };
    n += 4 + 20;
    if version >= 3 && method != 0 {
        n += 4 + 16 * nblocks as u64;
    }
    if version >= 3 {
        n += 1 + 4;
    }
    n
}

fn read_entry(c: &mut Cur, version: u32, mount: &str) -> Result<PakEntry> {
    let name = c.fstring()?;
    let offset = c.u64()?;
    let compressed_size = c.u64()?;
    let size = c.u64()?;
    let method = c.u32()?;
    let timestamp = if version < 2 { Some(c.i64()?) } else { None };
    let mut hash = [0u8; 20];
    hash.copy_from_slice(c.take(20)?);
    let mut blocks = Vec::new();
    if version >= 3 && method != 0 {
        let n = c.u32()? as usize;
        if n > 1 << 24 {
            return Err(PakError::Truncated("absurd compression block count".into()));
        }
        for _ in 0..n {
            let s = c.u64()?;
            let e = c.u64()?;
            // v>=5 stores offsets relative to the entry header start; normalise to absolute.
            if version >= 5 {
                blocks.push((s + offset, e + offset));
            } else {
                blocks.push((s, e));
            }
        }
    }
    let (mut encrypted, mut block_size) = (false, 0);
    if version >= 3 {
        encrypted = c.u8()? != 0;
        block_size = c.u32()?;
    }
    Ok(PakEntry {
        path: format!("{mount}{name}"),
        offset,
        compressed_size,
        size,
        method,
        hash,
        blocks,
        encrypted,
        block_size,
        timestamp,
    })
}

// ----------------------------------------------------------------------------------------------------
// PakFile
// ----------------------------------------------------------------------------------------------------

pub struct PakFile {
    /// File name only (`pakchunk0-WindowsClient.pak`).
    pub name: String,
    pub path: PathBuf,
    pub info: PakInfo,
    pub mount: String,
    pub entries: Vec<PakEntry>,
    #[cfg(not(target_arch = "wasm32"))]
    map: Mmap,
    /// BROWSER PORT: no memory maps in the browser; the file's size, its bytes are read by range ([`web`]).
    #[cfg(target_arch = "wasm32")]
    len: u64,
}

impl fmt::Debug for PakFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PakFile({} v{} {} entries)", self.name, self.info.version, self.entries.len())
    }
}

impl PakFile {
    pub fn open(path: impl AsRef<Path>) -> Result<PakFile> {
        let path = path.as_ref().to_path_buf();
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        #[cfg(target_arch = "wasm32")]
        return web::open(path, name);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let file = File::open(&path)?;
            // SAFETY: the pak is opened read-only and never modified by this process.
            let map = unsafe { Mmap::map(&file)? };
            let info = read_footer(&map, &name)?;
            if info.encrypted_index {
                return Err(PakError::EncryptedIndex(name));
            }
            let end = info.index_offset.checked_add(info.index_size).unwrap_or(u64::MAX);
            if end > map.len() as u64 {
                return Err(PakError::Truncated(format!("{name}: index beyond end of file")));
            }
            let (mount, entries) = parse_index(&map[info.index_offset as usize..end as usize], info.version)?;
            Ok(PakFile { name, path, info, mount, entries, map })
        }
    }

    /// Raw bytes of the whole file (memory mapped). Not available in the browser build (no memory maps; read entries instead).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn bytes(&self) -> &[u8] {
        &self.map
    }

    /// Read an entry's uncompressed bytes. Stored entries are returned zero-copy.
    pub fn read(&self, e: &PakEntry) -> Result<Cow<'_, [u8]>> {
        READ_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if e.encrypted {
            #[cfg(not(target_arch = "wasm32"))]
            if e.method == 0 { if let Some(key) = aes_key() { return self.read_encrypted(e, key); } }
            return Err(PakError::Encrypted(e.path.clone()));
        }
        #[cfg(target_arch = "wasm32")]
        {
            // an entry a `retain_reads` scope read earlier (e.g. on a loading thread) is not fetched again
            if retained::active() { if let Some(b) = retained::get(&self.name, e.offset) { return Ok(Cow::Owned(b.to_vec())); } }
            else if let Some(b) = retained::take(&self.name, e.offset) { return Ok(Cow::Owned(std::sync::Arc::try_unwrap(b).unwrap_or_else(|b| b.to_vec()))); }
            if let Some(ix) = webcook::index() {
                if let Some(l) = ix.find(&self.name, e.offset).filter(|l| l.size as u64 == e.size) {
                    match ix.read_entry(l) {
                        Ok(b) => {
                            if retained::active() { retained::put(&self.name, e.offset, b.clone()); }
                            read_log::note(&e.path, e.size);
                            return Ok(Cow::Owned(b));
                        }
                        Err(x) => eprintln!("[webcook] {}: {x} (pak read instead)", e.path),
                    }
                }
            }
            let r = web::read(self, e)?;
            if retained::active() { retained::put(&self.name, e.offset, r.to_vec()); }
            read_log::note(&e.path, if e.method == 0 { e.size } else { e.blocks.iter().map(|b| b.1.saturating_sub(b.0)).sum() });
            return Ok(r);
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.read_mapped(e)
    }

    /// A stored encrypted entry: the data is padded to 16 bytes and every 16-byte block is decrypted on its own (AES-256 ECB).
    #[cfg(not(target_arch = "wasm32"))]
    fn read_encrypted(&self, e: &PakEntry, key: &[u8; 32]) -> Result<Cow<'_, [u8]>> {
        use aes::cipher::{BlockDecrypt, KeyInit};
        let start = e.offset + entry_header_len(self.info.version, 0, 0);
        let end = start + ((e.size + 15) & !15);
        if end > self.map.len() as u64 { return Err(PakError::Truncated(format!("{}: entry data past EOF", e.path))); }
        let mut out = self.map[start as usize..end as usize].to_vec();
        let cipher = aes::Aes256::new(key.into());
        for b in out.chunks_exact_mut(16) { cipher.decrypt_block(b.into()); }
        out.truncate(e.size as usize);
        Ok(Cow::Owned(out))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn read_mapped(&self, e: &PakEntry) -> Result<Cow<'_, [u8]>> {
        let m = &self.map[..];
        if e.method == 0 {
            let start = e.offset + entry_header_len(self.info.version, 0, 0);
            let end = start + e.size;
            if end > m.len() as u64 {
                return Err(PakError::Truncated(format!("{}: entry data past EOF", e.path)));
            }
            return Ok(Cow::Borrowed(&m[start as usize..end as usize]));
        }
        let name = if self.info.version >= 8 {
            self.info.compression_names.get(e.method as usize - 1).cloned().unwrap_or_default()
        } else if e.method == 1 {
            "Zlib".to_string()
        } else {
            return Err(PakError::UnsupportedCompression(e.method.to_string()));
        };
        if !name.eq_ignore_ascii_case("zlib") {
            return Err(PakError::UnsupportedCompression(name));
        }
        let mut out = Vec::with_capacity(e.size as usize);
        let bs = if e.block_size == 0 { e.size.max(1) } else { e.block_size as u64 };
        let mut remaining = e.size;
        for &(s, en) in &e.blocks {
            if en > m.len() as u64 || s > en {
                return Err(PakError::Truncated(format!("{}: bad block range", e.path)));
            }
            let want = remaining.min(bs) as usize;
            let chunk = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&m[s as usize..en as usize], want.max(1) + 16)
                .map_err(|er| PakError::Decompress(format!("{}: {:?}", e.path, er.status)))?;
            remaining = remaining.saturating_sub(chunk.len() as u64);
            out.extend_from_slice(&chunk);
        }
        if out.len() as u64 != e.size {
            return Err(PakError::Decompress(format!("{}: size {} != {}", e.path, out.len(), e.size)));
        }
        Ok(Cow::Owned(out))
    }
}

/// The mount point and the entries of an index (`index_offset .. index_offset + index_size` of the file).
fn parse_index(idx: &[u8], version: u32) -> Result<(String, Vec<PakEntry>)> {
    let mut c = Cur { b: idx, p: 0 };
    let mount = c.fstring()?;
    let count = c.i32()?;
    if count < 0 {
        return Err(PakError::Truncated("negative entry count".into()));
    }
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        entries.push(read_entry(&mut c, version, &mount)?);
    }
    Ok((mount, entries))
}

fn read_footer(m: &[u8], name: &str) -> Result<PakInfo> {
    let len = m.len();
    let tail_len = len.min(512);
    read_footer_tail(&m[len - tail_len..], name)
}

/// [`read_footer`] on the last (up to) 512 bytes of the file.
fn read_footer_tail(tail: &[u8], name: &str) -> Result<PakInfo> {
    let mut start = None;
    if tail.len() >= 44 {
        for i in (0..=tail.len() - 4).rev() {
            if u32::from_le_bytes(tail[i..i + 4].try_into().unwrap()) == PAK_MAGIC {
                // plausible version?
                if i + 8 <= tail.len() {
                    let v = u32::from_le_bytes(tail[i + 4..i + 8].try_into().unwrap());
                    if (1..=32).contains(&v) {
                        start = Some(i);
                        break;
                    }
                }
            }
        }
    }
    let i = start.ok_or_else(|| PakError::NoFooter(name.to_string()))?;
    let version = u32::from_le_bytes(tail[i + 4..i + 8].try_into().unwrap());
    if version > 8 {
        return Err(PakError::UnsupportedVersion(version, name.to_string()));
    }
    if i + 44 > tail.len() {
        return Err(PakError::NoFooter(name.to_string()));
    }
    let index_offset = u64::from_le_bytes(tail[i + 8..i + 16].try_into().unwrap());
    let index_size = u64::from_le_bytes(tail[i + 16..i + 24].try_into().unwrap());
    let mut index_hash = [0u8; 20];
    index_hash.copy_from_slice(&tail[i + 24..i + 44]);
    let encrypted_index = version >= 4 && i >= 1 && tail[i - 1] != 0;
    let mut compression_names = Vec::new();
    if version >= 8 {
        let rest = &tail[i + 44..];
        for chunk in rest.chunks_exact(32) {
            let end = chunk.iter().position(|&b| b == 0).unwrap_or(32);
            if end > 0 {
                compression_names.push(String::from_utf8_lossy(&chunk[..end]).into_owned());
            }
        }
    }
    Ok(PakInfo { version, index_offset, index_size, index_hash, encrypted_index, compression_names })
}

// ----------------------------------------------------------------------------------------------------
// BROWSER PORT: ranged reads instead of a memory map
// ----------------------------------------------------------------------------------------------------

/// The browser build has no memory maps: the footer, the index and each entry's bytes are read by range through `std::fs` (the browser
/// platform layer turns that into HTTP Range requests to the local launcher). Every thread keeps its own handle per pak, so concurrent readers
/// never share a file position. An entry is one read: its stored bytes, or the span of all its compressed blocks.
#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;
    use std::cell::RefCell;
    use std::io::{Read, Seek, SeekFrom};

    thread_local! {
        static FILES: RefCell<HashMap<PathBuf, std::fs::File>> = RefCell::new(HashMap::new());
    }

    pub(super) fn read_range(path: &Path, off: u64, len: usize) -> Result<Vec<u8>> {
        FILES.with(|f| {
            let mut f = f.borrow_mut();
            if !f.contains_key(path) {
                let file = std::fs::File::open(path)?;
                f.insert(path.to_path_buf(), file);
            }
            let file = f.get_mut(path).expect("inserted");
            file.seek(SeekFrom::Start(off))?;
            let mut v = vec![0u8; len];
            file.read_exact(&mut v)?;
            Ok(v)
        })
    }

    pub(super) fn open(path: PathBuf, name: String) -> Result<PakFile> {
        if let Some(Ok((len, tail, idx))) = crate::webcook::index().and_then(|ix| ix.pak_index(&name)) {
            let info = read_footer_tail(&tail, &name)?;
            if !info.encrypted_index && info.index_size as usize == idx.len() {
                let (mount, entries) = parse_index(&idx, info.version)?;
                return Ok(PakFile { name, path, info, mount, entries, len });
            }
        }
        let len = std::fs::metadata(&path)?.len();
        let tail_len = len.min(512);
        let tail = read_range(&path, len - tail_len, tail_len as usize)?;
        let info = read_footer_tail(&tail, &name)?;
        if info.encrypted_index {
            return Err(PakError::EncryptedIndex(name));
        }
        let end = info.index_offset.checked_add(info.index_size).unwrap_or(u64::MAX);
        if end > len {
            return Err(PakError::Truncated(format!("{name}: index beyond end of file")));
        }
        let idx = read_range(&path, info.index_offset, info.index_size as usize)?;
        let (mount, entries) = parse_index(&idx, info.version)?;
        Ok(PakFile { name, path, info, mount, entries, len })
    }

    pub(super) fn read<'a>(pf: &'a PakFile, e: &PakEntry) -> Result<Cow<'a, [u8]>> {
        if e.method == 0 {
            let start = e.offset + entry_header_len(pf.info.version, 0, 0);
            if start + e.size > pf.len {
                return Err(PakError::Truncated(format!("{}: entry data past EOF", e.path)));
            }
            return Ok(Cow::Owned(read_range(&pf.path, start, e.size as usize)?));
        }
        let name = if pf.info.version >= 8 {
            pf.info.compression_names.get(e.method as usize - 1).cloned().unwrap_or_default()
        } else if e.method == 1 {
            "Zlib".to_string()
        } else {
            return Err(PakError::UnsupportedCompression(e.method.to_string()));
        };
        if !name.eq_ignore_ascii_case("zlib") {
            return Err(PakError::UnsupportedCompression(name));
        }
        let (Some(first), Some(last)) = (e.blocks.first(), e.blocks.last()) else { return Ok(Cow::Owned(Vec::new())) };
        let (span_start, span_end) = (e.blocks.iter().map(|b| b.0).min().unwrap_or(first.0), e.blocks.iter().map(|b| b.1).max().unwrap_or(last.1));
        if span_end > pf.len || span_start > span_end {
            return Err(PakError::Truncated(format!("{}: bad block range", e.path)));
        }
        let span = read_range(&pf.path, span_start, (span_end - span_start) as usize)?;
        let mut out = Vec::with_capacity(e.size as usize);
        let bs = if e.block_size == 0 { e.size.max(1) } else { e.block_size as u64 };
        let mut remaining = e.size;
        for &(s, en) in &e.blocks {
            if s > en {
                return Err(PakError::Truncated(format!("{}: bad block range", e.path)));
            }
            let want = remaining.min(bs) as usize;
            let chunk = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&span[(s - span_start) as usize..(en - span_start) as usize], want.max(1) + 16)
                .map_err(|er| PakError::Decompress(format!("{}: {:?}", e.path, er.status)))?;
            remaining = remaining.saturating_sub(chunk.len() as u64);
            out.extend_from_slice(&chunk);
        }
        if out.len() as u64 != e.size {
            return Err(PakError::Decompress(format!("{}: size {} != {}", e.path, out.len(), e.size)));
        }
        Ok(Cow::Owned(out))
    }
}

// ----------------------------------------------------------------------------------------------------
// load order
// ----------------------------------------------------------------------------------------------------

/// Sort key defining mount priority. **Later = higher priority** (overrides earlier).
/// `pakchunk<N>[_s<M>]-<platform>.pak` orders by `(N, M)`; patch paks (`*_P.pak`) come after everything;
/// anything else sorts by name after the chunk paks.
pub fn pak_load_order(file_name: &str) -> (u8, u64, u64, String) {
    let lower = file_name.to_lowercase();
    let stem = lower.trim_end_matches(".pak");
    let patch = stem.ends_with("_p") || stem.contains("_p-") || stem.contains("_p_");
    if let Some(rest) = stem.strip_prefix("pakchunk") {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let n: u64 = digits.parse().unwrap_or(0);
        let after = &rest[digits.len()..];
        let s: u64 = after
            .strip_prefix("_s")
            .map(|r| r.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0))
            .unwrap_or(0);
        return (if patch { 2 } else { 0 }, n, s, lower);
    }
    (if patch { 2 } else { 1 }, 0, 0, lower)
}

// ----------------------------------------------------------------------------------------------------
// PakSet
// ----------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryRef {
    pub pak: u32,
    pub index: u32,
}

/// All paks of an install, mounted together.
///
/// MEMORY + LOAD TIME (2026-10-06): the mounted index (every entry of every pak + the lookup table, ~100 MB for the 1.8 install) is mounted once
/// per process: opening the same pak files again returns a cheap clone sharing that index instead of parsing it again (many subsystems open the
/// install on their own, some on the UI thread in play; see `MOUNTED`). A `PakSet` is immutable after opening, so a shared one reads exactly like
/// a fresh one, from any thread.
pub struct PakSet {
    inner: std::sync::Arc<Mounted>,
    /// paks that failed to open (name, error) - e.g. encrypted index or unsupported version
    pub skipped: Vec<(String, String)>,
}

impl Clone for PakSet {
    fn clone(&self) -> PakSet { PakSet { inner: self.inner.clone(), skipped: self.skipped.clone() } }
}

struct Mounted {
    paks: Vec<PakFile>,
    /// normalised path -> winning entry
    lookup: HashMap<String, EntryRef>,
    names: std::sync::OnceLock<HashMap<u64, Vec<(u32, u32)>>>,
}

/// BROWSER PORT (wasm32): entries read inside a [`retain_reads`] scope, kept for later reads of the same entries (LRU, [`retained::BUDGET`]).
#[cfg(target_arch = "wasm32")]
mod retained {
    use std::cell::Cell;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};
    thread_local! { static ON: Cell<bool> = const { Cell::new(false) }; }
    /// 256 MB: the POI streaming prefetch (fn-web stream_web.rs) keeps up to ~32 kinds read ahead of the main thread
    pub(super) const BUDGET: usize = 256 << 20;
    struct Cache { map: HashMap<(String, u64), Arc<Vec<u8>>>, order: VecDeque<(String, u64)>, bytes: usize }
    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
    pub(super) fn active() -> bool { ON.with(|c| c.get()) }
    pub(super) fn set(on: bool) -> bool { ON.with(|c| c.replace(on)) }
    pub(super) fn get(pak: &str, offset: u64) -> Option<Arc<Vec<u8>>> { CACHE.lock().ok()?.as_ref()?.map.get(&(pak.to_owned(), offset)).cloned() }
    pub(super) fn take(pak: &str, offset: u64) -> Option<Arc<Vec<u8>>> {
        let mut g = CACHE.lock().ok()?;
        let c = g.as_mut()?;
        let d = c.map.remove(&(pak.to_owned(), offset))?;
        c.bytes -= d.len();
        if c.order.len() > 4 * c.map.len() + 1024 { let m = &c.map; c.order.retain(|k| m.contains_key(k)); }
        Some(d)
    }
    pub(super) fn put(pak: &str, offset: u64, data: Vec<u8>) {
        let Ok(mut g) = CACHE.lock() else { return };
        let c = g.get_or_insert_with(|| Cache { map: HashMap::new(), order: VecDeque::new(), bytes: 0 });
        let key = (pak.to_owned(), offset);
        if c.map.contains_key(&key) || data.len() > BUDGET { return; }
        c.bytes += data.len();
        c.map.insert(key.clone(), Arc::new(data));
        c.order.push_back(key);
        while c.bytes > BUDGET { let Some(k) = c.order.pop_front() else { break }; if let Some(d) = c.map.remove(&k) { c.bytes -= d.len(); } }
    }
    pub(super) fn clear() { if let Ok(mut g) = CACHE.lock() { *g = None; } }
}

/// BROWSER PORT (wasm32, `FN_READLOG=1`): stored bytes and reads per pak entry fetched (not served from memory), for "what does a load read".
#[cfg(target_arch = "wasm32")]
mod read_log {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static ON: OnceLock<bool> = OnceLock::new();
    static LOG: Mutex<Option<HashMap<String, (u32, u64, u64)>>> = Mutex::new(None);
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static MARKS: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());
    fn on() -> bool { *ON.get_or_init(|| fn_live::var("FN_READLOG").is_ok_and(|v| v == "1")) }
    pub(super) fn note(path: &str, bytes: u64) {
        if !on() { return; }
        if let Ok(mut g) = LOG.lock() { let e = g.get_or_insert_with(HashMap::new).entry(path.to_owned()).or_insert_with(|| (0, 0, SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed))); e.0 += 1; e.1 += bytes; }
    }
    pub(super) fn mark(name: &str) { if on() { if let Ok(mut m) = MARKS.lock() { m.push((name.to_owned(), SEQ.load(std::sync::atomic::Ordering::Relaxed))); } } }
    pub(super) fn report(top: usize) -> String {
        let Ok(g) = LOG.lock() else { return String::new() };
        let Some(m) = g.as_ref() else { return "pak read log: empty (FN_READLOG=1 not set?)
".into() };
        let strip = |p: &str| p.trim_start_matches("../").trim_start_matches("FortniteGame/Content/").to_owned();
        let total: u64 = m.values().map(|v| v.1).sum();
        let mut out = format!("pak reads: {} entries, {} reads, {:.1} MB stored bytes
", m.len(), m.values().map(|v| v.0 as u64).sum::<u64>(), total as f64 / 1048576.);
        let mut groups: HashMap<String, (u32, u64)> = HashMap::new();
        for (p, v) in m { let s = strip(p); let g: Vec<&str> = s.split('/').take(3).collect(); let e = groups.entry(g[..g.len().saturating_sub(1).max(1)].join("/")).or_default(); e.0 += 1; e.1 += v.1; }
        let mut gv: Vec<_> = groups.into_iter().collect();
        gv.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
        out.push_str("by folder (entries, MB):
");
        for (g, (n, b)) in gv.iter().take(top) { out.push_str(&format!("  {:>8.1} MB {:>6}  {g}
", *b as f64 / 1048576., n)); }
        let mut ev: Vec<_> = m.iter().collect();
        ev.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
        out.push_str("largest entries (MB, reads):
");
        let top = if fn_live::var("FN_READLOG_ALL").is_ok_and(|v| v == "1") { usize::MAX } else { top };
        for (p, (n, b, _)) in ev.iter().take(top) { out.push_str(&format!("  {:>8.2} MB {:>3}x  {}
", *b as f64 / 1048576., n, strip(p))); }
        if fn_live::var("FN_READLOG_ALL").is_ok_and(|v| v == "1") {
            if let Ok(mk) = MARKS.lock() { for (n, q) in mk.iter() { out.push_str(&format!("mark {q} {n}
")); } }
            let mut ov: Vec<_> = m.iter().collect();
            ov.sort_by_key(|a| a.1 .2);
            out.push_str("read order:
");
            for (p, v) in ov { out.push_str(&format!("  {} {}
", v.2, strip(p))); }
        }
        out
    }
}

/// BROWSER PORT: the `FN_READLOG=1` report: what the pak reads of this session were, by folder and largest entries (empty natively).
pub fn read_log_mark(name: &str) {
    #[cfg(target_arch = "wasm32")]
    read_log::mark(name);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = name;
}

pub fn read_log_report(top: usize) -> String {
    #[cfg(target_arch = "wasm32")]
    return read_log::report(top);
    #[cfg(not(target_arch = "wasm32"))]
    { let _ = top; String::new() }
}

/// LOAD TIME (browser build): the pak entries `f` reads on this thread are kept in memory (LRU, 256 MB) so later reads of the same entries do not
/// fetch them again; e.g. a loading thread reads what the page's main thread will need, where every read waits for the network. Natively a plain
/// call (the paks are memory-mapped).
pub fn retain_reads<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(target_arch = "wasm32")]
    {
        let prev = retained::set(true);
        let r = f();
        retained::set(prev);
        r
    }
    #[cfg(not(target_arch = "wasm32"))]
    f()
}

pub fn without_retain<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(target_arch = "wasm32")]
    {
        let prev = retained::set(false);
        let r = f();
        retained::set(prev);
        r
    }
    #[cfg(not(target_arch = "wasm32"))]
    f()
}

/// Drops what [`retain_reads`] kept (browser build; natively nothing is kept).
pub fn drop_retained_reads() {
    #[cfg(target_arch = "wasm32")]
    retained::clear();
}

/// The mounted indexes of this process by their (load-ordered) file list; see [`PakSet`]. A mount stays for the whole process: every later open
/// of the same paks is a cheap clone sharing it (`Arc`). Subsystems open the install on their own (`PakSet::open_install` in ~20 places of the game,
/// several on the UI thread in play: a picked-up weapon, the first dropped weapon, the storm, the scope ...), and re-parsing the 238k-entry index
/// cost 0.1-0.2 s natively and ~1 s in the browser each time a previous set had just been dropped. ~100 MB, which some `PakSet` holds during play
/// anyway. A pak file is never modified while the game runs (and a mapped file cannot be rewritten on Windows).
static MOUNTED: std::sync::Mutex<Vec<(Vec<PathBuf>, std::sync::Arc<Mounted>, Vec<(String, String)>)>> = std::sync::Mutex::new(Vec::new());

/// A thread that asks for paks another thread is mounting right now waits for that mount instead of parsing the same index in parallel.
mod mounting {
    use super::PathBuf;
    use std::sync::{Condvar, Mutex};
    static MOUNTING: Mutex<Vec<Vec<PathBuf>>> = Mutex::new(Vec::new());
    static DONE: Condvar = Condvar::new();
    /// This thread mounts `.0`; dropping it (success, error or panic) wakes the threads waiting for the same files.
    pub(super) struct Mounting(Vec<PathBuf>);
    impl Drop for Mounting {
        fn drop(&mut self) {
            if let Ok(mut g) = MOUNTING.lock() { g.retain(|f| f != &self.0); }
            DONE.notify_all();
        }
    }
    /// `Some`: nobody else is mounting `files`, this thread does. `None`: another thread was; it has finished (its result is in `MOUNTED`
    /// unless it failed).
    pub(super) fn begin(files: &[PathBuf]) -> Option<Mounting> {
        let mut g = MOUNTING.lock().unwrap_or_else(|e| e.into_inner());
        if !g.iter().any(|f| f.as_slice() == files) { g.push(files.to_vec()); return Some(Mounting(files.to_vec())); }
        while g.iter().any(|f| f.as_slice() == files) { g = DONE.wait(g).unwrap_or_else(|e| e.into_inner()); }
        None
    }
}

impl PakSet {
    /// Open an install. `dir` may be a directory containing `.pak` files, a `Content/Paks` directory's
    /// parent (`FortniteGame`), or any ancestor (searched up to 6 levels deep for a `Paks` directory).
    pub fn open_install(dir: impl AsRef<Path>) -> Result<PakSet> {
        let paks_dir = find_paks_dir(dir.as_ref())
            .ok_or_else(|| PakError::Other(format!("no .pak files found under {}", dir.as_ref().display())))?;
        Self::open_dir(paks_dir)
    }

    /// Open every `*.pak` directly inside `dir`.
    pub fn open_dir(dir: impl AsRef<Path>) -> Result<PakSet> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir.as_ref())?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x.eq_ignore_ascii_case("pak")).unwrap_or(false))
            .collect();
        Self::open_files_inner(&mut files)
    }

    pub fn open_files(files: &[PathBuf]) -> Result<PakSet> {
        let mut f = files.to_vec();
        Self::open_files_inner(&mut f)
    }

    /// The mounted index of exactly these (load-ordered) files, if any.
    fn live_mount(files: &[PathBuf]) -> Option<PakSet> {
        let m = MOUNTED.lock().ok()?;
        let (_, inner, skipped) = m.iter().find(|(f, ..)| f.as_slice() == files)?;
        Some(PakSet { inner: inner.clone(), skipped: skipped.clone() })
    }

    fn open_files_inner(files: &mut Vec<PathBuf>) -> Result<PakSet> {
        files.sort_by_key(|p| pak_load_order(&p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()));
        if let Some(set) = Self::live_mount(files) {
            return Ok(set);
        }
        // a mount of the same files in progress on another thread: wait for it, then use it (or mount here if it failed)
        let _mounting = loop {
            if let Some(g) = mounting::begin(files) { break g; }
            if let Some(set) = Self::live_mount(files) { return Ok(set); }
        };
        // parse paks in parallel (index parsing dominates)
        let results: Vec<Result<PakFile>> = std::thread::scope(|s| {
            let handles: Vec<_> = files.iter().map(|p| s.spawn(move || PakFile::open(p))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(PakError::Other("pak open thread panicked".into())))).collect()
        });
        let mut paks = Vec::new();
        let mut skipped = Vec::new();
        for (p, r) in files.iter().zip(results) {
            match r {
                Ok(pf) => paks.push(pf),
                Err(e) => skipped.push((p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), e.to_string())),
            }
        }
        if paks.is_empty() {
            return Err(PakError::Other(format!("no readable paks ({} skipped: {:?})", skipped.len(), skipped)));
        }
        let mut lookup: HashMap<String, EntryRef> = HashMap::with_capacity(paks.iter().map(|p| p.entries.len()).sum());
        for (pi, pf) in paks.iter().enumerate() {
            for (ei, e) in pf.entries.iter().enumerate() {
                lookup.insert(e.key(), EntryRef { pak: pi as u32, index: ei as u32 });
            }
        }
        let inner = std::sync::Arc::new(Mounted { paks, lookup, names: std::sync::OnceLock::new() });
        if let Ok(mut m) = MOUNTED.lock() {
            // (only this thread mounts these files: `mounting::begin`; keep the first one regardless)
            if let Some((_, shared, _)) = m.iter().find(|(f, ..)| f == files) {
                return Ok(PakSet { inner: shared.clone(), skipped });
            }
            m.push((files.clone(), inner.clone(), skipped.clone()));
        }
        Ok(PakSet { inner, skipped })
    }

    /// Paks in load order (last wins).
    pub fn paks(&self) -> &[PakFile] {
        &self.inner.paks
    }
    /// Number of distinct (effective) paths.
    pub fn len(&self) -> usize {
        self.inner.lookup.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.lookup.is_empty()
    }
    /// Total entries across all paks, including shadowed duplicates.
    pub fn total_entries(&self) -> usize {
        self.inner.paks.iter().map(|p| p.entries.len()).sum()
    }

    pub fn entry(&self, r: EntryRef) -> &PakEntry {
        &self.inner.paks[r.pak as usize].entries[r.index as usize]
    }

    /// Case-insensitive lookup of the winning entry.
    pub fn find(&self, path: &str) -> Option<(&PakFile, &PakEntry)> {
        let r = self.inner.lookup.get(&normalize_path(path))?;
        let pf = &self.inner.paks[r.pak as usize];
        Some((pf, &pf.entries[r.index as usize]))
    }
    pub fn contains(&self, path: &str) -> bool {
        self.inner.lookup.contains_key(&normalize_path(path))
    }

    /// Read the winning entry's bytes (zero-copy for stored entries).
    pub fn read(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        let (pf, e) = self.find(path).ok_or_else(|| PakError::NotFound(path.to_string()))?;
        pf.read(e)
    }

    /// Read a long package path's file (`/Game/Foo` + `"uasset"`).
    pub fn read_game(&self, long: &str, ext: &str) -> Result<Cow<'_, [u8]>> {
        let p = game_path_to_pak(long, ext).ok_or_else(|| PakError::NotFound(long.to_string()))?;
        self.read(&p)
    }

    /// Winning entries whose engine-relative path matches `pattern` (see [`glob_match`]); a pattern without
    /// `/` matches the file name only. Sorted by path.
    pub fn glob(&self, pattern: &str) -> Vec<(&PakFile, &PakEntry)> {
        let pat = pattern.replace('\\', "/");
        let pat = pat.trim_start_matches("../").trim_start_matches('/').to_string();
        let name_only = !pat.contains('/');
        // PERF (browser port): the literal text after the last wildcard must end every match (keys are lower case): a cheap test before the
        // full match, same results
        let lower = pat.to_lowercase();
        let tail = match lower.rfind(['*', '?']) { Some(i) => &lower[i + 1..], None => &lower[..] };
        let mut out: Vec<(&PakFile, &PakEntry)> = self
            .inner.lookup
            .iter()
            .filter(|(k, _)| {
                if name_only {
                    let name = k.rsplit('/').next().unwrap_or(k);
                    name.ends_with(tail) && glob_match(&pat, name)
                } else {
                    k.ends_with(tail) && glob_match(&pat, k)
                }
            })
            .map(|(_, r)| {
                let pf = &self.inner.paks[r.pak as usize];
                (pf, &pf.entries[r.index as usize])
            })
            .collect();
        out.sort_by(|a, b| a.1.path.to_lowercase().cmp(&b.1.path.to_lowercase()));
        out
    }

    pub fn by_file_name(&self, name: &str) -> Vec<(&PakFile, &PakEntry)> {
        fn h(s: &str) -> u64 { use std::hash::{Hash, Hasher}; let mut d = std::collections::hash_map::DefaultHasher::new(); s.hash(&mut d); d.finish() }
        let idx = self.inner.names.get_or_init(|| { let mut m: HashMap<u64, Vec<(u32, u32)>> = HashMap::new(); for (k, r) in &self.inner.lookup { m.entry(h(k.rsplit('/').next().unwrap_or(k))).or_default().push((r.pak, r.index)); } m });
        let want = name.to_lowercase();
        let mut out: Vec<(&PakFile, &PakEntry)> = idx.get(&h(&want)).map(|v| v.iter().map(|&(p, i)| { let pf = &self.inner.paks[p as usize]; (pf, &pf.entries[i as usize]) }).filter(|(_, e)| { let n = e.path.replace('\\', "/"); n.rsplit('/').next().unwrap_or(&n).to_lowercase() == want }).collect()).unwrap_or_default();
        out.sort_by(|a, b| a.1.path.to_lowercase().cmp(&b.1.path.to_lowercase()));
        out
    }

    /// Iterate winning entries (unordered).
    pub fn effective(&self) -> impl Iterator<Item = (&PakFile, &PakEntry)> {
        self.inner.lookup.values().map(move |r| {
            let pf = &self.inner.paks[r.pak as usize];
            (pf, &pf.entries[r.index as usize])
        })
    }

    /// Iterate every entry of every pak in load order (including shadowed ones).
    pub fn all_entries(&self) -> impl Iterator<Item = (&PakFile, &PakEntry)> {
        self.inner.paks.iter().flat_map(|p| p.entries.iter().map(move |e| (p, e)))
    }
}

fn dir_has_pak(d: &Path) -> bool {
    std::fs::read_dir(d)
        .map(|rd| rd.filter_map(|e| e.ok()).any(|e| e.path().extension().map(|x| x.eq_ignore_ascii_case("pak")).unwrap_or(false)))
        .unwrap_or(false)
}

/// Locate the `Content/Paks` directory of an install given the install root, `FortniteGame`, or `Paks` itself.
pub fn find_paks_dir(root: &Path) -> Option<PathBuf> {
    if dir_has_pak(root) {
        return Some(root.to_path_buf());
    }
    for rel in ["Content/Paks", "FortniteGame/Content/Paks", "Fortnite/FortniteGame/Content/Paks"] {
        let c = root.join(rel);
        if dir_has_pak(&c) {
            return Some(c);
        }
    }
    fn walk(d: &Path, depth: u32) -> Option<PathBuf> {
        if depth == 0 {
            return None;
        }
        for e in std::fs::read_dir(d).ok()?.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().map(|n| n.eq_ignore_ascii_case("Paks")).unwrap_or(false) && dir_has_pak(&p) {
                    return Some(p);
                }
                if let Some(r) = walk(&p, depth - 1) {
                    return Some(r);
                }
            }
        }
        None
    }
    walk(root, 6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_basics() {
        assert!(glob_match("*.uasset", "foo.uasset"));
        assert!(!glob_match("*.uasset", "a/foo.uasset"));
        assert!(glob_match("**/*.uasset", "a/b/foo.uasset"));
        assert!(glob_match("a/**/foo.uasset", "a/foo.uasset"));
        assert!(glob_match("A/*/Foo.U?sset", "a/b/foo.uasset"));
        assert!(!glob_match("a/*", "a/b/c"));
        assert!(glob_match("a/**", "a/b/c"));
    }

    /// The allocation-free ASCII matcher gives exactly the char matcher's answers.
    #[test]
    fn ascii_glob_matches_the_char_glob() {
        let pats = ["*.uasset", "**/*.uasset", "a/**/foo.uasset", "A/*/Foo.U?sset", "a/*", "a/**", "fortnitegame/content/**/m_nowep_relaxed.uasset", "?", "*", "**", "**/", "a?b", "*/b/*", "x**y", "**/a/**/b"];
        let texts = ["foo.uasset", "a/foo.uasset", "a/b/foo.uasset", "a/b/c", "FortniteGame/Content/Animation/M_NoWep_Relaxed.uasset", "fortnitegame/content/x/m_nowep_relaxed.uexp", "", "a", "/", "a/b", "axb", "a/b/c/b", "xy", "x/y", "a/q/b"];
        for p in pats {
            for t in texts {
                let slow = glob_rec(&p.to_lowercase().chars().collect::<Vec<_>>(), &t.to_lowercase().chars().collect::<Vec<_>>());
                assert_eq!(glob_match(p, t), slow, "{p} vs {t}");
            }
        }
    }

    #[test]
    fn paths() {
        assert_eq!(normalize_path("../../../FortniteGame\\Content/X.uasset"), "fortnitegame/content/x.uasset");
        assert_eq!(game_path_to_pak("/Game/Athena/Foo.Foo_C", "uasset").unwrap(), "FortniteGame/Content/Athena/Foo.uasset");
        assert_eq!(pak_path_to_game("../../../FortniteGame/Content/Athena/Foo.uasset").unwrap(), "/Game/Athena/Foo");
    }

    #[test]
    fn order() {
        let mut v = vec!["pakchunk1-WindowsClient.pak", "pakchunk0_s2-WindowsClient.pak", "pakchunk0-WindowsClient.pak", "pakchunk0_s1-WindowsClient.pak"];
        v.sort_by_key(|n| pak_load_order(n));
        assert_eq!(v, ["pakchunk0-WindowsClient.pak", "pakchunk0_s1-WindowsClient.pak", "pakchunk0_s2-WindowsClient.pak", "pakchunk1-WindowsClient.pak"]);
    }
}


/// The pak AES-256 key from `FN_PAK_AES_KEY` (64 hex digits, optional `0x`), parsed once. None when unset or malformed.
#[cfg(not(target_arch = "wasm32"))]
pub fn aes_key() -> Option<&'static [u8; 32]> {
    static KEY: std::sync::OnceLock<Option<[u8; 32]>> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        let v = std::env::var("FN_PAK_AES_KEY").ok()?;
        let h = v.trim().trim_start_matches("0x").trim_start_matches("0X");
        if h.len() != 64 { return None; }
        let mut k = [0u8; 32];
        for i in 0..32 { k[i] = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()?; }
        Some(k)
    }).as_ref()
}
