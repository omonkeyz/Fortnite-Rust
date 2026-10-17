//! Where the game's files come from.
//!
//! The runtime finds every file **by name** (case-insensitive, no directory) in an ordered list of asset roots, like the old prototype:
//!
//! 1. `--assets <dir>`, then env `FN_ASSETS`;
//! 2. `assets` beside the executable (the shipping layout: `fortnite-rs.exe` + `assets\`);
//! 3. a cache produced from the paks (`--paks`, see [`export_from_paks`]), inserted *before* the plain asset root so freshly exported files win.
//!
//! There are no baked-in developer paths. When nothing usable is found the caller shows [`describe_search`] (every place that was looked at) in a native
//! message box.
//!
//! `--paks <Paks dir | install dir>` (or env `FN_PAKS` / `FN_ROOT`, or `Paks` beside the executable) is used to *fill in what the asset tree lacks*:
//! the paks are mounted with `fn-pak` only to validate the install; the actual extraction (meshes -> glTF, textures -> PNG, animations -> md5anim,
//! sky meshes) is done by the `fn-export` tool shipped beside the game (`fn-export --md5`), whose output tree uses the same layout the loaders read.
use fn_ui::assets::{AssetSource, DirAssets};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Files the game cannot start without (looked up by name in the roots).
pub const REQUIRED: &[&str] = &["SK_M_Med_Soldier_04.gltf", "SK_M_MED_Head_01_ATH.gltf", "SK_Pick.gltf", "M_Melee_TwoHanded.md5anim", "T_Grasslands_D.png", "dome_optimized.bin", "sunmoon.bin"];
/// Nice to have: the game degrades without them (no splash artwork / hud art).
pub const OPTIONAL: &[&str] = &["Splash.bmp"];

pub struct AssetRoots {
    pub roots: Vec<PathBuf>,
    /// lower-cased file name -> path (first root wins).
    index: HashMap<String, PathBuf>,
    /// Every file in priority order (what `fn_legacy::find` searches).
    pub files: Vec<PathBuf>,
}

impl AssetRoots {
    pub fn open(roots: Vec<PathBuf>) -> AssetRoots {
        let mut index = HashMap::new();
        let mut files = Vec::new();
        for r in &roots {
            for f in fn_legacy::files(r) {
                if let Some(n) = f.file_name() { index.entry(n.to_string_lossy().to_ascii_lowercase()).or_insert_with(|| f.clone()); }
                files.push(f);
            }
        }
        AssetRoots { roots, index, files }
    }
    pub fn find(&self, name: &str) -> Option<&Path> { self.index.get(&name.to_ascii_lowercase()).map(|p| p.as_path()) }
    pub fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        let p = self.find(name).ok_or_else(|| format!("Missing asset: {name}"))?;
        std::fs::read(p).map_err(|e| format!("Cannot read {}: {e}", p.display()))
    }
    pub fn load_rgba(&self, name: &str) -> Result<image::RgbaImage, String> {
        let bytes = self.read(name)?;
        image::load_from_memory(&bytes).map(|i| i.to_rgba8()).map_err(|e| format!("Cannot decode {name}: {e}"))
    }
    /// Names of [`REQUIRED`] files that no root provides.
    pub fn missing_required(&self) -> Vec<&'static str> { REQUIRED.iter().copied().filter(|n| self.find(n).is_none()).collect() }
    pub fn len(&self) -> usize { self.files.len() }
    /// The UI's file source over all roots (first root wins, like `find`).
    pub fn ui_source(&self) -> Box<dyn AssetSource> { Box::new(MultiAssets { dirs: self.roots.iter().map(|r| DirAssets::new(r.clone())).collect() }) }
}

/// `fn_ui::assets::AssetSource` over several directory roots.
pub struct MultiAssets { dirs: Vec<DirAssets> }
impl AssetSource for MultiAssets {
    fn read(&self, name: &str) -> Option<Vec<u8>> { self.dirs.iter().find_map(|d| d.read(name)) }
    fn read_in(&self, name: &str, dir_hint: &str) -> Option<Vec<u8>> {
        // a hint that matches in some root wins over a hint-less hit in an earlier root
        let hint = dir_hint.to_ascii_lowercase().replace('\\', "/");
        for d in &self.dirs { if let Some(p) = d.path_of(name) { if p.to_string_lossy().to_ascii_lowercase().replace('\\', "/").contains(&hint) { return d.read_in(name, dir_hint); } } }
        self.dirs.iter().find_map(|d| d.read_in(name, dir_hint))
    }
    fn names(&self) -> Vec<String> { let mut v: Vec<String> = self.dirs.iter().flat_map(|d| d.names()).collect(); v.sort(); v.dedup(); v }
}

pub fn exe_dir() -> PathBuf { std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from(".")) }

/// Where asset roots are looked for, in priority order, as `(what, path)`: `--assets`, env `FN_ASSETS`, `assets` beside the executable. Nothing else:
/// there are no baked-in developer paths (the old fallbacks to a Downloads folder and the prototype's tree are gone).
pub fn root_locations(explicit: Option<&Path>, env_assets: Option<PathBuf>, exe_dir: &Path) -> Vec<(&'static str, PathBuf)> {
    let mut c = vec![];
    if let Some(p) = explicit { c.push(("--assets", p.to_path_buf())); }
    if let Some(p) = env_assets { c.push(("FN_ASSETS", p)); }
    c.push(("beside the executable", exe_dir.join("assets")));
    c
}
/// Candidate asset roots in priority order (existing directories only, canonicalised, de-duplicated).
pub fn candidate_roots(explicit: Option<&Path>) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for (_, p) in root_locations(explicit, std::env::var_os("FN_ASSETS").map(PathBuf::from), &exe_dir()) {
        let Ok(canon) = p.canonicalize() else { continue };
        if canon.is_dir() && !seen.contains(&canon) { seen.push(canon); }
    }
    seen
}

/// Where a Fortnite install's Paks folder may be, `(what, path)`: `--paks`, env `FN_PAKS`, env `FN_ROOT` (an install folder), `Paks` beside the executable.
pub fn paks_locations(explicit: Option<&Path>, env_paks: Option<PathBuf>, env_root: Option<PathBuf>, exe_dir: &Path) -> Vec<(&'static str, PathBuf)> {
    let mut c = vec![];
    if let Some(p) = explicit { c.push(("--paks", p.to_path_buf())); }
    if let Some(p) = env_paks { c.push(("FN_PAKS", p)); }
    if let Some(p) = env_root { c.push(("FN_ROOT", p)); }
    c.push(("beside the executable", exe_dir.join("Paks")));
    c
}
pub fn candidate_paks(explicit: Option<&Path>) -> Vec<PathBuf> {
    paks_locations(explicit, std::env::var_os("FN_PAKS").map(PathBuf::from), std::env::var_os("FN_ROOT").map(PathBuf::from), &exe_dir()).into_iter().map(|(_, p)| p).filter(|p| p.is_dir()).collect()
}

/// "Looked in:" lines for the missing-assets message: every location that was searched and whether it exists / what it held.
pub fn describe_search(explicit_assets: Option<&Path>, explicit_paks: Option<&Path>) -> String {
    let exe = exe_dir();
    let mut s = String::from("Looked for the asset folder in:\n");
    for (what, p) in root_locations(explicit_assets, std::env::var_os("FN_ASSETS").map(PathBuf::from), &exe) {
        s.push_str(&format!("  {what}: {} ({})\n", p.display(), if p.is_dir() { "exists" } else { "not found" }));
    }
    if std::env::var_os("FN_ASSETS").is_none() { s.push_str("  FN_ASSETS: not set\n"); }
    s.push_str("Looked for Fortnite 1.8 pak files in:\n");
    for (what, p) in paks_locations(explicit_paks, std::env::var_os("FN_PAKS").map(PathBuf::from), std::env::var_os("FN_ROOT").map(PathBuf::from), &exe) {
        s.push_str(&format!("  {what}: {} ({})\n", p.display(), if find_paks_dir(&p).is_some() { "pak files found" } else if p.is_dir() { "no .pak files" } else { "not found" }));
    }
    for var in ["FN_PAKS", "FN_ROOT"] { if std::env::var_os(var).is_none() { s.push_str(&format!("  {var}: not set\n")); } }
    s.push_str(&crate::paths::describe(&exe));
    s
}
/// A valid Paks directory: it (or `FortniteGame/Content/Paks` below it) holds `.pak` files.
pub fn find_paks_dir(dir: &Path) -> Option<PathBuf> {
    let has_pak = |d: &Path| std::fs::read_dir(d).map_or(false, |rd| rd.flatten().any(|e| e.path().extension().map_or(false, |x| x.eq_ignore_ascii_case("pak"))));
    for sub in ["", "FortniteGame/Content/Paks", "Fortnite/FortniteGame/Content/Paks", "Content/Paks"] {
        let d = if sub.is_empty() { dir.to_path_buf() } else { dir.join(sub) };
        if d.is_dir() && has_pak(&d) { return Some(d); }
    }
    None
}

/// Mounts the install with `fn-pak` (validation + stats). Returns (pak files, entries).
pub fn probe_paks(paks_dir: &Path) -> Result<(usize, usize), String> {
    let set = fn_pak::PakSet::open_install(paks_dir).map_err(|e| format!("cannot open the paks in {}: {e}", paks_dir.display()))?;
    Ok((set.paks().len(), set.total_entries()))
}

/// Runs `fn-export --md5` into `cache` (the pak -> glTF/PNG/md5anim/sky pipeline of the `fn-export` crate). `progress` receives its stdout lines.
pub fn export_from_paks(paks_dir: &Path, manifest: &Path, cache: &Path, mut progress: impl FnMut(&str)) -> Result<(), String> {
    let exe = find_fn_export();
    std::fs::create_dir_all(cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    let mut child = std::process::Command::new(&exe)
        .args(["--paks".as_ref(), paks_dir.as_os_str(), "--manifest".as_ref(), manifest.as_os_str(), "--out".as_ref(), cache.as_os_str(), "--md5".as_ref(), "all".as_ref()])
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::inherit()).spawn()
        .map_err(|e| format!("cannot start fn-export ({}: {e}); build it with `cargo build -p fn-export --release`, keep it beside the game, or set FN_EXPORT, or use --assets", exe.display()))?;
    if let Some(out) = child.stdout.take() {
        use std::io::BufRead;
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) { progress(&line); }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("fn-export exited with {status}")) }
}

/// The manifest `fn-export` needs: `--manifest`, env `FN_MANIFEST`, `asset-manifest.json` beside the executable, then a
/// development checkout's `tools/asset-manifest.json` (and `asset-manifest.json`) relative to the current directory.
pub fn find_manifest(explicit: Option<&Path>) -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = Vec::new();
    if let Some(p) = explicit { c.push(p.to_path_buf()); }
    if let Some(p) = std::env::var_os("FN_MANIFEST") { c.push(PathBuf::from(p)); }
    c.push(exe_dir().join("asset-manifest.json"));
    c.push(PathBuf::from("tools").join("asset-manifest.json"));
    c.push(PathBuf::from("asset-manifest.json"));
    c.into_iter().find(|p| p.is_file())
}

/// Where the `fn-export` executable may be, in priority order: beside the game, `FN_EXPORT`, then the sibling cargo
/// profile directories of a development checkout (`target/<profile>`), then bare `fn-export` (resolved through `PATH`).
pub fn fn_export_locations(exe_dir: &Path, env_export: Option<PathBuf>) -> Vec<PathBuf> {
    let name = if cfg!(windows) { "fn-export.exe" } else { "fn-export" };
    let mut c = vec![exe_dir.join(name)];
    if let Some(p) = env_export { c.push(p); }
    if let Some(target) = exe_dir.parent() {
        for profile in ["release", "debug"] { c.push(target.join(profile).join(name)); }
    }
    c.push(PathBuf::from(name));
    c
}

/// The `fn-export` executable to run: the first existing candidate of [`fn_export_locations`] (bare `fn-export` when
/// none exists, so the spawn error names it).
pub fn find_fn_export() -> PathBuf {
    fn_export_locations(&exe_dir(), std::env::var_os("FN_EXPORT").map(PathBuf::from)).into_iter().find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "fn-export.exe" } else { "fn-export" }))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(name: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("fn-app-test-{name}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test] fn lookup_is_case_insensitive_and_earlier_roots_win() {
        let (a, b) = (tmp("a"), tmp("b"));
        std::fs::create_dir_all(a.join("x")).unwrap(); std::fs::write(a.join("x").join("Thing.PNG"), b"a").unwrap(); std::fs::write(b.join("thing.png"), b"b").unwrap(); std::fs::write(b.join("only-b.txt"), b"c").unwrap();
        let r = AssetRoots::open(vec![a.clone(), b.clone()]);
        assert_eq!(r.read("thing.png").unwrap(), b"a"); assert_eq!(r.read("ONLY-B.TXT").unwrap(), b"c");
        assert!(r.find("nope").is_none() && r.read("nope").unwrap_err().contains("Missing asset: nope"));
        assert_eq!(r.ui_source().read("Thing.png").unwrap(), b"a"); assert_eq!(r.len(), 3);
        let _ = (std::fs::remove_dir_all(a), std::fs::remove_dir_all(b));
    }
    #[test] fn locations_are_explicit_env_then_beside_the_exe_and_nothing_baked_in() {
        let exe = Path::new("C:/game");
        let l = root_locations(Some(Path::new("X")), Some(PathBuf::from("E")), exe);
        assert_eq!(l.iter().map(|(w, p)| (*w, p.clone())).collect::<Vec<_>>(), [("--assets", PathBuf::from("X")), ("FN_ASSETS", PathBuf::from("E")), ("beside the executable", exe.join("assets"))]);
        assert_eq!(root_locations(None, None, exe).len(), 1);
        let p = paks_locations(Some(Path::new("P")), Some(PathBuf::from("FP")), Some(PathBuf::from("FR")), exe);
        assert_eq!(p.iter().map(|(w, _)| *w).collect::<Vec<_>>(), ["--paks", "FN_PAKS", "FN_ROOT", "beside the executable"]);
        // no developer path may be compiled into the binary's discovery code
        let src = include_str!("assets.rs");
        for needle in [["Users", "Brady"].join("\\"), ["Fortnite", "Runtime", "assets"].join("\\"), "CARGO_MANIFEST_DIR\")).join".to_owned()] { assert!(!src.split("mod tests").next().unwrap().contains(&needle), "baked developer path {needle}"); }
    }
    #[test] fn fn_export_locations_prefer_beside_the_exe_then_env_then_the_dev_target() {
        let exe = Path::new("/g/target/fast");
        let l = fn_export_locations(exe, Some(PathBuf::from("/opt/fn-export")));
        let name = if cfg!(windows) { "fn-export.exe" } else { "fn-export" };
        assert_eq!(l, vec![exe.join(name), PathBuf::from("/opt/fn-export"), Path::new("/g/target/release").join(name), Path::new("/g/target/debug").join(name), PathBuf::from(name)]);
        let bare = fn_export_locations(Path::new("."), None);
        assert_eq!(bare.first(), Some(&PathBuf::from(".").join(name)));
        assert_eq!(bare.last(), Some(&PathBuf::from(name)));
    }
    #[test] fn the_search_description_names_every_place_and_its_state() {
        let d = describe_search(Some(Path::new("Z:/definitely/not/here")), None);
        assert!(d.contains("--assets") && d.contains("not found") && d.contains("beside the executable") && d.contains("pak files"), "{d}");
    }
    #[test] fn missing_required_lists_what_is_absent() { let d = tmp("m"); std::fs::write(d.join("SK_Pick.gltf"), b"x").unwrap(); let r = AssetRoots::open(vec![d.clone()]); let m = r.missing_required(); assert!(!m.contains(&"SK_Pick.gltf") && m.contains(&"T_Grasslands_D.png")); let _ = std::fs::remove_dir_all(d); }
    #[test] fn paks_dir_detection_accepts_the_install_or_the_paks_folder() {
        let d = tmp("p"); let paks = d.join("FortniteGame/Content/Paks"); std::fs::create_dir_all(&paks).unwrap(); std::fs::write(paks.join("pakchunk0.pak"), b"x").unwrap();
        assert_eq!(find_paks_dir(&paks).as_deref(), Some(paks.as_path())); assert_eq!(find_paks_dir(&d).as_deref(), Some(paks.as_path())); assert!(find_paks_dir(&d.join("FortniteGame")).is_some());
        assert!(find_paks_dir(&tmp("empty")).is_none()); let _ = std::fs::remove_dir_all(d);
    }
}
