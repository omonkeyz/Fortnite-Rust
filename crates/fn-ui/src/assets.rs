//! CPU-side texture store and asset access for the UI. No GPU types: backends read `TextureStore` and upload what changed.
//!
//! * [`TextureStore`] hands out [`TextureId`]s; each [`Texture`] keeps STRAIGHT RGBA8 plus sampler hints and a `version` that bumps when the
//!   pixels change (text atlases, baked materials). Backends upload `Texture::premultiplied()` for quads and the raw straight pixels for textures
//!   sampled inside material shaders.
//! * [`AssetSource`] abstracts where files come from. [`DirAssets`] indexes a directory tree by (case-insensitive) file name, which is how the
//!   old runtime located assets (`assets::find`). `pak_assets::PakAssets` (feature `pak`) reads the Fortnite install directly (fn-pak + fn-texture, textures
//!   decoded on demand) and [`LayeredAssets`] stacks sources (install first, exported folder as the fallback).
//! * [`UiAssets`] = store + source + record of missing optional textures (`note_missing`).
use crate::draw::TextureId;
use std::{collections::HashMap, path::{Path, PathBuf}, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexInfo { pub id: TextureId, pub w: u32, pub h: u32 }
impl TexInfo { pub const NONE: TexInfo = TexInfo { id: TextureId::NONE, w: 1, h: 1 }; }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Filter { Linear, Nearest }

/// How a backend should treat the texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sampler {
    pub filter: Filter,
    /// Build a mip chain (box/triangle filtered in PREMULTIPLIED space, as the old runtime did for HUD art).
    pub mips: bool,
    /// Repeat instead of clamp-to-edge (only material samplers use it).
    pub wrap: bool,
}
impl Sampler {
    pub const UI_ART: Sampler = Sampler { filter: Filter::Linear, mips: true, wrap: false };
    pub const LINEAR: Sampler = Sampler { filter: Filter::Linear, mips: false, wrap: false };
    pub const NEAREST: Sampler = Sampler { filter: Filter::Nearest, mips: false, wrap: false };
}

#[derive(Clone)]
pub struct Texture {
    pub w: u32,
    pub h: u32,
    /// STRAIGHT (non-premultiplied) RGBA8, row 0 = top.
    pub rgba: Arc<Vec<u8>>,
    pub sampler: Sampler,
    pub version: u32,
    pub cpu: Arc<std::sync::atomic::AtomicBool>,
}
impl Texture {
    pub fn new(w: u32, h: u32, rgba: Vec<u8>, sampler: Sampler) -> Self { assert_eq!(rgba.len(), (w * h * 4) as usize); Texture { w, h, rgba: Arc::new(rgba), sampler, version: 0, cpu: Default::default() } }
    pub fn has_pixels(&self) -> bool { self.rgba.len() >= (self.w * self.h * 4) as usize }
    pub fn mark_cpu(&self) { self.cpu.store(true, std::sync::atomic::Ordering::Relaxed); }
    /// Premultiplied copy: `c = (c * a + 127) / 255` (identical to the old runtime's `premultiply`).
    pub fn premultiplied(&self) -> Vec<u8> { premultiply_rgba8(&self.rgba) }
}
pub fn premultiply_rgba8(px: &[u8]) -> Vec<u8> {
    let mut o = px.to_vec();
    for p in o.chunks_exact_mut(4) { let a = p[3] as u32; for c in 0..3 { p[c] = ((p[c] as u32 * a + 127) / 255) as u8; } }
    o
}

/// Decoded image (straight RGBA8) as read from the asset source.
#[derive(Clone)]
pub struct Image { pub w: u32, pub h: u32, pub rgba: Vec<u8> }

pub struct TextureStore { slots: Vec<Option<Texture>>, names: HashMap<String, TextureId> }
impl Default for TextureStore { fn default() -> Self { Self::new() } }
impl TextureStore {
    pub fn new() -> Self {
        let mut s = TextureStore { slots: vec![None], names: HashMap::new() };
        let white = s.insert(Texture::new(1, 1, vec![255; 4], Sampler::NEAREST));
        debug_assert_eq!(white, TextureId::WHITE);
        s
    }
    pub fn insert(&mut self, t: Texture) -> TextureId { self.slots.push(Some(t)); TextureId(self.slots.len() as u32 - 1) }
    pub fn insert_named(&mut self, name: &str, t: Texture) -> TextureId { let id = self.insert(t); self.names.insert(name.to_ascii_lowercase(), id); id }
    pub fn named(&self, name: &str) -> Option<TextureId> { self.names.get(&name.to_ascii_lowercase()).copied() }
    pub fn get(&self, id: TextureId) -> Option<&Texture> { self.slots.get(id.0 as usize).and_then(|s| s.as_ref()) }
    pub fn info(&self, id: TextureId) -> TexInfo { self.get(id).map(|t| TexInfo { id, w: t.w, h: t.h }).unwrap_or(TexInfo::NONE) }
    /// Replace the pixels (same or new size) and bump the version.
    pub fn replace(&mut self, id: TextureId, w: u32, h: u32, rgba: Vec<u8>) {
        if let Some(Some(t)) = self.slots.get_mut(id.0 as usize) { let v = t.version.wrapping_add(1); *t = Texture { w, h, rgba: Arc::new(rgba), sampler: t.sampler, version: v, cpu: t.cpu.clone() }; }
    }
    pub fn name_of(&self, id: TextureId) -> Option<&str> { self.names.iter().find(|(_, v)| **v == id).map(|(k, _)| k.as_str()) }
    pub fn evict_pixels(&mut self, ids: &[u32], min_px: u64) -> (usize, u64) {
        let (mut n, mut bytes) = (0usize, 0u64);
        for &i in ids {
            let id = TextureId(i);
            if i <= 1 || self.name_of(id).is_none() { continue; }
            let Some(Some(t)) = self.slots.get_mut(i as usize) else { continue };
            if (t.w as u64) * (t.h as u64) < min_px || !t.has_pixels() || t.cpu.load(std::sync::atomic::Ordering::Relaxed) || Arc::strong_count(&t.rgba) > 1 { continue; }
            bytes += t.rgba.len() as u64;
            t.rgba = Arc::new(Vec::new());
            n += 1;
        }
        (n, bytes)
    }
    pub fn remove(&mut self, id: TextureId) { if id.0 > 1 { if let Some(s) = self.slots.get_mut(id.0 as usize) { *s = None; } } }
    pub fn len(&self) -> usize { self.slots.iter().filter(|s| s.is_some()).count() }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
    /// (id, texture) of every live texture, for backends that sync after each frame.
    pub fn iter(&self) -> impl Iterator<Item = (TextureId, &Texture)> + '_ { self.slots.iter().enumerate().filter_map(|(i, s)| s.as_ref().map(|t| (TextureId(i as u32), t))) }
}

/// INVENTORY FIX3 (inventory_gaps G2): the asset name of a cooked texture OBJECT path, keeping its long package path so the lookup cannot pick another
/// package with the same stem: `/Game/UI/Style/BasicStrokeBox.BasicStrokeBox` -> `/Game/UI/Style/BasicStrokeBox.png` (`/Game/UI/Main/BasicStrokeBox` is a
/// different, non-texture package). A path without a long package part stays a plain file name (`Name.png`).
pub fn texture_name_of(object_path: &str) -> String {
    let pkg = object_path.split('.').next().unwrap_or(object_path);
    if pkg.starts_with('/') { format!("{pkg}.png") } else { format!("{}.png", object_path.rsplit('.').next().unwrap_or(object_path)) }
}
/// The file name part of an asset name (`/Game/UI/Style/BasicStrokeBox.png` -> `BasicStrokeBox.png`; a plain name is returned as is).
pub fn file_name_of(name: &str) -> &str { name.rsplit(['/', '\\']).next().unwrap_or(name) }

/// Where UI files come from.
pub trait AssetSource {
    /// Raw bytes by file NAME (case-insensitive, no directory), e.g. `"Button-Default-M-_normal_.png"`. A name may also be a long package path
    /// (`"/Game/UI/Style/BasicStrokeBox.png"`, see [`texture_name_of`]): that exact package (pak source) or the file whose path ends with it (folders).
    fn read(&self, name: &str) -> Option<Vec<u8>>;
    /// Like `read`, but only files whose path contains `dir_hint` (forward-slash, case-insensitive), to disambiguate duplicates.
    fn read_in(&self, name: &str, dir_hint: &str) -> Option<Vec<u8>> { let _ = dir_hint; self.read(name) }
    /// All file names known to the source (for diagnostics/tests).
    fn names(&self) -> Vec<String> { Vec::new() }
    /// Decoded image (straight RGBA8) by file name. The default decodes the bytes of [`AssetSource::read`] as PNG/BMP; a source that already holds
    /// pixels (the pak source decodes textures itself) overrides it to skip the encode/decode round trip.
    fn image(&self, name: &str) -> Option<Image> { self.read(name).and_then(|b| decode_image(&b).ok()) }
}

/// Several sources tried in order: the first one that has a file wins (install first, exported folder as the fallback).
pub struct LayeredAssets { layers: Vec<Box<dyn AssetSource>> }
impl LayeredAssets {
    pub fn new(layers: Vec<Box<dyn AssetSource>>) -> Self { LayeredAssets { layers } }
    pub fn len(&self) -> usize { self.layers.len() }
    pub fn is_empty(&self) -> bool { self.layers.is_empty() }
}
impl AssetSource for LayeredAssets {
    fn read(&self, name: &str) -> Option<Vec<u8>> { self.layers.iter().find_map(|l| l.read(name)) }
    fn read_in(&self, name: &str, dir_hint: &str) -> Option<Vec<u8>> { self.layers.iter().find_map(|l| l.read_in(name, dir_hint)) }
    fn names(&self) -> Vec<String> { let mut v: Vec<String> = self.layers.iter().flat_map(|l| l.names()).collect(); v.sort(); v.dedup(); v }
    fn image(&self, name: &str) -> Option<Image> { self.layers.iter().find_map(|l| l.image(name)) }
}

/// Known locations of the old runtime tree (the real one first).
pub const OLD_ROOT_CANDIDATES: &[&str] = &[
    r"C:\Users\Brady\Desktop\Fortnite Rust\FortniteRuntime",
    r"C:\Users\Brady\Documents\Codex\2026-10-02\im-seeing-titkoks-where-people-are-2\FortniteRuntime",
];
/// Directory tree indexed by lower-cased file name (first match wins, deterministic order).
pub struct DirAssets { root: PathBuf, files: HashMap<String, Vec<PathBuf>> }
impl DirAssets {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into(); let mut files: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            let mut ents: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(); ents.sort();
            for p in ents { if p.is_dir() { stack.push(p); } else if let Some(n) = p.file_name() { files.entry(n.to_string_lossy().to_ascii_lowercase()).or_default().push(p); } }
        }
        for v in files.values_mut() { v.sort(); }
        DirAssets { root, files }
    }
    pub fn root(&self) -> &Path { &self.root }
    pub fn path_of(&self, name: &str) -> Option<&PathBuf> {
        // INVENTORY FIX3 (G2): a long package path picks the file whose path ends with it (`.../Game/UI/Style/BasicStrokeBox.png`), else the first of that name
        let file = file_name_of(name);
        let v = self.files.get(&file.to_ascii_lowercase())?;
        if file.len() != name.len() {
            let tail = name.trim_start_matches(['/', '\\']).to_ascii_lowercase().replace('\\', "/");
            if let Some(p) = v.iter().find(|p| p.to_string_lossy().to_ascii_lowercase().replace('\\', "/").ends_with(&tail)) { return Some(p); }
        }
        v.first()
    }
    /// Default asset root of the OLD runtime tree (set `FN_UI_ASSETS` to override).
    pub fn default_root() -> PathBuf {
        if let Some(p) = fn_live::var_os("FN_UI_ASSETS") { return PathBuf::from(p); }
        if let Some(p) = fn_live::var_os("FN_UI_OLD_ROOT") { let a = PathBuf::from(p).join("assets"); if a.is_dir() { return a; } }
        Self::old_root_candidates().into_iter().map(|r| r.join("assets")).find(|a| a.is_dir())
            .unwrap_or_else(|| PathBuf::from(OLD_ROOT_CANDIDATES[0]).join("assets"))
    }
    /// Existing candidate roots of the old runtime tree (`FN_UI_OLD_ROOT` first).
    pub fn old_root_candidates() -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = Vec::new();
        if let Some(p) = fn_live::var_os("FN_UI_OLD_ROOT") { v.push(PathBuf::from(p)); }
        v.extend(OLD_ROOT_CANDIDATES.iter().map(PathBuf::from));
        v.retain(|p| p.is_dir()); v
    }
    /// Root of the old runtime tree: `FN_UI_OLD_ROOT`, else the first existing known location.
    pub fn old_root() -> PathBuf { Self::old_root_candidates().into_iter().next().unwrap_or_else(|| PathBuf::from(OLD_ROOT_CANDIDATES[0])) }
}
impl AssetSource for DirAssets {
    fn read(&self, name: &str) -> Option<Vec<u8>> { self.path_of(name).and_then(|p| std::fs::read(p).ok()) }
    fn read_in(&self, name: &str, dir_hint: &str) -> Option<Vec<u8>> {
        let hint = dir_hint.to_ascii_lowercase().replace('\\', "/");
        let v = self.files.get(&file_name_of(name).to_ascii_lowercase())?;
        let p = v.iter().find(|p| p.to_string_lossy().to_ascii_lowercase().replace('\\', "/").contains(&hint)).or(v.first())?;
        std::fs::read(p).ok()
    }
    fn names(&self) -> Vec<String> { self.files.keys().cloned().collect() }
}

/// In-memory source (tests).
#[derive(Default)]
pub struct MemAssets(pub HashMap<String, Vec<u8>>);
impl AssetSource for MemAssets { fn read(&self, name: &str) -> Option<Vec<u8>> { self.0.get(&name.to_ascii_lowercase()).or_else(|| self.0.get(&file_name_of(name).to_ascii_lowercase())).cloned() } }

pub fn decode_image(bytes: &[u8]) -> Result<Image, String> {
    let im = image::load_from_memory(bytes).map_err(|e| e.to_string())?.to_rgba8();
    Ok(Image { w: im.width(), h: im.height(), rgba: im.into_raw() })
}

pub struct UiAssets {
    pub store: TextureStore,
    pub source: Box<dyn AssetSource>,
    pub missing: Vec<String>,
    cache: HashMap<String, TexInfo>,
}
impl UiAssets {
    pub fn new(source: Box<dyn AssetSource>) -> Self { UiAssets { store: TextureStore::new(), source, missing: Vec::new(), cache: HashMap::new() } }
    pub fn from_dir(root: impl Into<PathBuf>) -> Self { Self::new(Box::new(DirAssets::new(root))) }
    pub fn note_missing(&mut self, what: &str) { if !self.missing.iter().any(|m| m == what) { self.missing.push(what.to_owned()); } }
    /// Decode a file by name without registering it (CPU materials sample these).
    pub fn image(&mut self, name: &str) -> Option<Image> {
        match self.source.image(name) { Some(i) => Some(i), None => { self.note_missing(&format!("Missing asset: {name}")); None } }
    }
    /// Load (once) a UI art texture by file name (mip-mapped, clamped). Missing -> `TexInfo::NONE` and recorded in `missing`.
    pub fn texture(&mut self, name: &str) -> TexInfo { self.texture_with(name, Sampler::UI_ART) }
    pub fn texture_with(&mut self, name: &str, sampler: Sampler) -> TexInfo {
        let key = format!("{}|{:?}", name.to_ascii_lowercase(), sampler);
        if let Some(t) = self.cache.get(&key) { return *t; }
        let info = match self.image(name) {
            Some(i) => { let id = self.store.insert_named(name, Texture::new(i.w, i.h, i.rgba, sampler)); TexInfo { id, w: i.w, h: i.h } }
            None => TexInfo::NONE,
        };
        self.cache.insert(key, info); info
    }
    pub fn release_large(&mut self, min_px: u64) -> (usize, u64) { self.release_large_keep(min_px, &[]) }
    pub fn release_large_keep(&mut self, min_px: u64, keep: &[TextureId]) -> (usize, u64) {
        let big: Vec<(String, TexInfo)> = self.cache.iter().filter(|(_, t)| t.id != TextureId::NONE && t.w as u64 * t.h as u64 >= min_px && !keep.contains(&t.id)).map(|(k, t)| (k.clone(), *t)).collect();
        let mut bytes = 0u64;
        for (k, t) in &big { self.cache.remove(k); bytes += t.w as u64 * t.h as u64 * 4; self.store.names.retain(|_, id| *id != t.id); self.store.remove(t.id); }
        (big.len(), bytes)
    }
    pub fn white(&self) -> TextureId { TextureId::WHITE }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn store_has_white_at_one() { let s = TextureStore::new(); assert_eq!(s.info(TextureId::WHITE).w, 1); assert!(s.get(TextureId::NONE).is_none()); }
    #[test] fn premultiply_matches_old_rounding() { let t = Texture::new(1, 1, vec![200, 100, 50, 128], Sampler::LINEAR); assert_eq!(t.premultiplied(), vec![((200u32 * 128 + 127) / 255) as u8, ((100u32 * 128 + 127) / 255) as u8, ((50u32 * 128 + 127) / 255) as u8, 128]); }
    #[test] fn missing_texture_is_recorded_once() { let mut a = UiAssets::new(Box::new(MemAssets::default())); assert_eq!(a.texture("nope.png").id, TextureId::NONE); a.texture("nope.png"); a.texture_with("nope.png", Sampler::LINEAR); assert_eq!(a.missing.len(), 1); }
    #[test] fn layered_sources_try_in_order_and_fall_back() {
        let png = |v: u8| { use image::ImageEncoder; let mut o = Vec::new(); image::codecs::png::PngEncoder::new(&mut o).write_image(&[v, v, v, 255], 1, 1, image::ExtendedColorType::Rgba8).unwrap(); o };
        let a = MemAssets([("a.png".to_owned(), png(10))].into_iter().collect());
        let b = MemAssets([("a.png".to_owned(), png(20)), ("b.png".to_owned(), png(30))].into_iter().collect());
        let l = LayeredAssets::new(vec![Box::new(a), Box::new(b)]);
        assert_eq!(l.image("A.png").unwrap().rgba[0], 10, "the first layer wins");
        assert_eq!(l.image("b.png").unwrap().rgba[0], 30, "the second layer fills the gap");
        assert!(l.image("c.png").is_none() && l.read("c.png").is_none());
        let mut ua = UiAssets::new(Box::new(l));
        assert!(ua.texture("b.png").id != TextureId::NONE && ua.texture("c.png").id == TextureId::NONE && ua.missing == ["Missing asset: c.png"]);
    }
    #[test] fn replace_bumps_version() { let mut s = TextureStore::new(); let id = s.insert(Texture::new(1, 1, vec![0; 4], Sampler::LINEAR)); s.replace(id, 2, 1, vec![1; 8]); let t = s.get(id).unwrap(); assert_eq!((t.w, t.version), (2, 1)); }
}
