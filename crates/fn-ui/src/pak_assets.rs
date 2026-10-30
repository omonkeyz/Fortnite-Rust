//! Pak-backed UI asset source: the UI runs from the Fortnite install directly (textures decoded on demand with `fn-texture`, fonts read as raw
//! `.ufont` / `.ttf` bytes), with [`LayeredAssets`] to fall back to an exported folder for anything the install does not provide.
//!
//! * **Textures.** The UI asks for files by name (`"Button-Default-M-_normal_.png"`, case-insensitive). [`PakAssets`] maps that name to a package of
//!   the paks: first through the table copied from the old prototype's `tools/asset-manifest.json` (`data/ui-texture-map.tsv`, the packages the UI
//!   textures were exported from, which also resolves the `-64` -> `-XL` aliases of 1.10 names), then, for names the table does not know, through a
//!   scan of the pak index for `<name>.uasset` (preferring `/Game/UI/`). The package is read from the paks, parsed with `fn-ueio` and decoded with
//!   `fn-texture` (UEViewer-compatible decoding, so the pixels are the ones the exported PNGs hold). Nothing is cached on disk and nothing is
//!   decoded until a screen asks for it; `UiAssets` keeps the decoded texture.
//! * **Fonts and other files.** Everything that is not a `.png` is looked up by file name in the pak index (`NotoSans-Regular.ufont`,
//!   `BurbankBigCondensed-Black.ufont`, `Roboto-Bold.ttf`, ...) and returned as stored.
//! * The install is read-only: nothing here writes, and the game is never run.
use crate::assets::{AssetSource, Image};
use fn_pak::PakSet;
use fn_texture::{DecodeOptions, Texture2D};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const TEXTURE_MAP: &str = include_str!("../data/ui-texture-map.tsv");

/// Default install location of the 1.8 client the runtime is built from (`FN_ROOT` overrides: the `FortniteGame` directory).
pub fn default_install() -> PathBuf {
    if let Some(p) = fn_live::var_os("FN_ROOT") { return PathBuf::from(p); }
    PathBuf::from(r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame")
}

/// Name (lower case, with extension) -> package path (`Game/...`), from the embedded table.
fn texture_map() -> &'static HashMap<String, String> {
    static M: OnceLock<HashMap<String, String>> = OnceLock::new();
    M.get_or_init(|| {
        TEXTURE_MAP.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .filter_map(|l| l.split_once('\t')).map(|(n, p)| (n.trim().to_ascii_lowercase(), p.trim().to_owned())).collect()
    })
}

/// Names the UI still uses from the 1.10 era that are called differently in 1.8 (file stems, lower case): the loading screen's wordmark file kept its
/// old `1024x512` name while the 1.8 package is `T-Logo-Wordmark-1024`.
const TEXTURE_ALIASES: [(&str, &str); 1] = [("t-logo-wordmark-1024x512", "t-logo-wordmark-1024")];

/// File extensions served as raw bytes by name (fonts).
const RAW_EXTS: [&str; 5] = ["ufont", "ttf", "otf", "ini", "locres"];

pub struct PakAssets {
    set: PakSet,
    /// `<package file stem>` (lower case) -> pak paths of its `.uasset` (only built when a name is not in the embedded table).
    stems: OnceLock<HashMap<String, Vec<String>>>,
    /// file name (lower case, with extension) -> pak paths, for the raw (font) files.
    raw: OnceLock<HashMap<String, Vec<String>>>,
}

impl PakAssets {
    /// Open the install (`dir` may be the `Paks` directory, `FortniteGame` or any directory above it).
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, String> {
        let set = PakSet::open_install(dir.as_ref()).map_err(|e| format!("cannot open the paks under {}: {e}", dir.as_ref().display()))?;
        Ok(Self::from_set(set))
    }
    pub fn from_set(set: PakSet) -> Self { PakAssets { set, stems: OnceLock::new(), raw: OnceLock::new() } }
    pub fn pak_set(&self) -> &PakSet { &self.set }

    fn stems(&self) -> &HashMap<String, Vec<String>> {
        self.stems.get_or_init(|| {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for (_, e) in self.set.effective() {
                let p = e.rel_path();
                if let Some(base) = p.strip_suffix(".uasset") {
                    let stem = base.rsplit('/').next().unwrap_or(base).to_ascii_lowercase();
                    m.entry(stem).or_default().push(base.to_owned());
                }
            }
            for v in m.values_mut() { v.sort_by_key(|p| (!p.to_ascii_lowercase().contains("/ui/"), p.len(), p.to_ascii_lowercase())); }
            m
        })
    }
    fn raw(&self) -> &HashMap<String, Vec<String>> {
        self.raw.get_or_init(|| {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for (_, e) in self.set.effective() {
                let p = e.rel_path();
                let name = p.rsplit('/').next().unwrap_or(p).to_ascii_lowercase();
                if name.rsplit('.').next().map_or(false, |x| RAW_EXTS.contains(&x)) { m.entry(name).or_default().push(p.to_owned()); }
            }
            for v in m.values_mut() { v.sort(); }
            m
        })
    }

    /// The long package path (`/Game/UI/Style/Tab1-M-_active_`) a `.png` name is served from, if the paks have it.
    pub fn package_of(&self, name: &str) -> Option<String> {
        let lower = name.to_ascii_lowercase();
        let stem = lower.strip_suffix(".png")?;
        // INVENTORY FIX3 (inventory_gaps G2): a long package path (`/Game/UI/Style/BasicStrokeBox.png`, `assets::texture_name_of`) is that package and no other
        // (two packages share the stem `BasicStrokeBox`: `UI/Main/` is not a texture, `UI/Style/` is the one `M-BasicStrokeBox` samples)
        if stem.starts_with('/') {
            let long = &name[..name.len() - 4];
            return fn_pak::game_path_to_pak(long, "uasset").filter(|p| self.set.contains(p)).map(|_| long.to_owned());
        }
        if let Some(pkg) = texture_map().get(&lower) {
            let long = format!("/{pkg}");
            if fn_pak::game_path_to_pak(&long, "uasset").map_or(false, |p| self.set.contains(&p)) { return Some(long); }
        }
        // not in the table: the first package of that name in the index (a pak path is `FortniteGame/Content/UI/..`: back to `/Game/UI/..`)
        let alias = TEXTURE_ALIASES.iter().find(|(from, _)| *from == stem).map(|(_, to)| *to);
        let pak_path = self.stems().get(alias.unwrap_or(stem))?.first()?;
        fn_pak::pak_path_to_game(&format!("{pak_path}.uasset")).map(|g| g.trim_end_matches(".uasset").to_owned())
    }

    /// Decode the texture package behind `name` (largest available mip, straight RGBA8, row 0 = top).
    pub fn decode_texture(&self, name: &str) -> Result<Image, String> {
        let Some(long) = self.package_of(name) else {
            // not a cooked texture package: the engine's own Slate images ship as plain PNG files (`Engine/Content/Slate/Common/Throbber_Piece.png`, ...)
            let hit = self.set.glob(crate::assets::file_name_of(name)).into_iter().find(|(_, e)| e.rel_path().to_ascii_lowercase().starts_with("engine/content/slate/")).map(|(_, e)| e.rel_path().to_owned());
            let bytes = hit.as_ref().and_then(|p| self.set.read(p).ok()).ok_or_else(|| format!("{name}: no such texture in the paks"))?;
            return crate::assets::decode_image(&bytes).map_err(|e| format!("{name}: {e}"));
        };
        let uasset = self.set.read_game(&long, "uasset").map_err(|e| format!("{long}.uasset: {e}"))?.into_owned();
        let uexp = self.set.read_game(&long, "uexp").map(|c| c.into_owned()).unwrap_or_default();
        let ubulk = self.set.read_game(&long, "ubulk").map(|c| c.into_owned()).unwrap_or_default();
        let tex = Texture2D::from_files(&uasset, &uexp, &ubulk).map_err(|e| format!("{long}: {e}"))?;
        let mip = tex.best_mip().ok_or_else(|| format!("{long}: no mip with data"))?;
        let img = tex.decode(mip, DecodeOptions::default()).map_err(|e| format!("{long}: {e}"))?;
        Ok(Image { w: img.width as u32, h: img.height as u32, rgba: img.to_rgba8() })
    }

    fn raw_bytes(&self, name: &str, dir_hint: Option<&str>) -> Option<Vec<u8>> {
        let lower = name.to_ascii_lowercase();
        if self.raw.get().is_none() {
            if !lower.rsplit('.').next().is_some_and(|x| RAW_EXTS.contains(&x)) { return None; }
            let mut v: Vec<String> = self.set.by_file_name(&lower).into_iter().map(|(_, e)| e.rel_path().to_owned()).filter(|p| p.rsplit('/').next().unwrap_or(p).to_ascii_lowercase() == lower).collect();
            if v.is_empty() { return None; }
            v.sort();
            let hint = dir_hint.map(|h| h.to_ascii_lowercase().replace('\\', "/"));
            let p = hint.as_ref().and_then(|h| v.iter().find(|p| p.to_ascii_lowercase().contains(h.as_str()))).or(v.first())?;
            return self.set.read(p).ok().map(|c| c.into_owned());
        }
        let v = self.raw().get(&lower)?;
        let hint = dir_hint.map(|h| h.to_ascii_lowercase().replace('\\', "/"));
        let p = hint.as_ref().and_then(|h| v.iter().find(|p| p.to_ascii_lowercase().contains(h.as_str()))).or(v.first())?;
        self.set.read(p).ok().map(|c| c.into_owned())
    }
}

impl AssetSource for PakAssets {
    /// `.png` names: the decoded texture re-encoded as a lossless RGBA PNG (use [`AssetSource::image`] to skip the round trip); other names: the stored bytes.
    fn read(&self, name: &str) -> Option<Vec<u8>> {
        if name.to_ascii_lowercase().ends_with(".png") {
            let im = self.decode_texture(name).ok()?;
            let mut out = Vec::new();
            use image::ImageEncoder;
            image::codecs::png::PngEncoder::new(&mut out).write_image(&im.rgba, im.w, im.h, image::ExtendedColorType::Rgba8).ok()?;
            return Some(out);
        }
        self.raw_bytes(name, None)
    }
    fn read_in(&self, name: &str, dir_hint: &str) -> Option<Vec<u8>> {
        if name.to_ascii_lowercase().ends_with(".png") { self.read(name) } else { self.raw_bytes(name, Some(dir_hint)) }
    }
    fn image(&self, name: &str) -> Option<Image> {
        if name.to_ascii_lowercase().ends_with(".png") { self.decode_texture(name).ok() } else { self.read(name).and_then(|b| crate::assets::decode_image(&b).ok()) }
    }
    fn names(&self) -> Vec<String> { texture_map().keys().cloned().chain(self.raw().keys().cloned()).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{DirAssets, UiAssets};
    use crate::draw::TextureId;

    /// The install, or None (skip) when it is not on this machine.
    fn install() -> Option<&'static PakAssets> {
        static P: OnceLock<Option<PakAssets>> = OnceLock::new();
        P.get_or_init(|| {
            let dir = default_install();
            if !dir.exists() { eprintln!("skipping: Fortnite install {} not found (set FN_ROOT)", dir.display()); return None; }
            match PakAssets::open(&dir) { Ok(p) => Some(p), Err(e) => { eprintln!("skipping: {e}"); None } }
        }).as_ref()
    }

    #[test] fn the_embedded_map_parses_and_has_the_ui_textures() {
        let m = texture_map();
        assert!(m.len() >= 190, "{}", m.len());
        assert_eq!(m.get("button-default-m-_normal_.png").map(String::as_str), Some("Game/UI/Style/Button-Default-M-_normal_"));
        assert_eq!(m.get("t-icon-exit-64.png"), m.get("t-icon-exit-xl.png"), "the 1.10 alias names resolve to the 1.8 package");
        assert!(m.keys().all(|k| k.ends_with(".png") && k == &k.to_ascii_lowercase()));
    }
    #[test] fn names_of_files_that_are_not_textures_are_not_packages() {
        // (pure: no install needed) a name without the .png suffix never resolves to a texture package
        let Some(p) = install() else { return };
        assert!(p.package_of("NotoSans-Regular.ufont").is_none());
        assert!(p.package_of("definitely-not-a-ui-texture.png").is_none());
        assert!(p.decode_texture("definitely-not-a-ui-texture.png").is_err());
    }
    /// INVENTORY FIX3 (inventory_gaps G2): `BasicStrokeBox` is two packages (`UI/Main/` is not a texture); the long package path picks the Texture2D
    /// (256x64 DXT5) that `M-BasicStrokeBox` samples, and a long path that is not a package resolves to nothing.
    #[test] fn a_long_package_path_picks_that_package() {
        let Some(p) = install() else { return };
        assert_eq!(p.package_of("/Game/UI/Style/BasicStrokeBox.png").as_deref(), Some("/Game/UI/Style/BasicStrokeBox"));
        let im = p.decode_texture("/Game/UI/Style/BasicStrokeBox.png").expect("UI/Style/BasicStrokeBox decodes");
        assert_eq!((im.w, im.h), (256, 64));
        assert!(p.package_of("/Game/UI/Style/NoSuchTexture.png").is_none());
        assert_eq!(crate::assets::texture_name_of("/Game/UI/Style/BasicStrokeBox.BasicStrokeBox"), "/Game/UI/Style/BasicStrokeBox.png");
    }
    #[test] fn every_mapped_texture_is_in_the_paks_and_decodes() {
        let Some(p) = install() else { return };
        let mut bad = Vec::new();
        for name in texture_map().keys() {
            match p.decode_texture(name) {
                Ok(im) => if im.w == 0 || im.h == 0 || im.rgba.len() != (im.w * im.h * 4) as usize { bad.push(format!("{name}: bad size {}x{}", im.w, im.h)); },
                Err(e) => bad.push(format!("{name}: {e}")),
            }
        }
        // (textures the manifest lists but 1.8 does not ship are reported by fn-export too; the UI never asks for them)
        assert!(bad.len() <= 3, "{} textures failed: {:?}", bad.len(), bad);
    }
    #[test] fn fonts_come_straight_from_the_paks_and_equal_the_exported_files() {
        let Some(p) = install() else { return };
        let dir = DirAssets::new(DirAssets::default_root().join("fonts"));
        for name in ["NotoSans-Regular.ufont", "NotoSans-Bold.ufont", "BurbankBigCondensed-Black.ufont"] {
            let a = p.read(name).unwrap_or_else(|| panic!("{name} not in the paks"));
            if let Some(b) = dir.read(name) { assert_eq!(a, b, "{name}: bytes differ from the exported file"); }
            assert!(crate::text::parse_font(a).is_ok(), "{name} parses as a font");
        }
        let a = p.read_in("Roboto-Bold.ttf", "/Slate/Fonts/").expect("Roboto-Bold.ttf in the paks");
        assert!(crate::text::parse_font(a).is_ok());
    }
    /// A texture that is in the manifest and in the exported folder decodes to the very same pixels.
    #[test] fn pak_textures_equal_the_exported_pngs_pixel_for_pixel() {
        let Some(p) = install() else { return };
        let root = DirAssets::default_root();
        if !root.is_dir() { eprintln!("skipping: no exported folder"); return; }
        let dir = DirAssets::new(&root);
        let (mut checked, mut diffs) = (0, Vec::new());
        for name in texture_map().keys() {
            let Some(old) = dir.image(name) else { continue };
            let Ok(new) = p.decode_texture(name) else { continue };
            checked += 1;
            if (old.w, old.h) != (new.w, new.h) { diffs.push(format!("{name}: size {}x{} vs {}x{}", old.w, old.h, new.w, new.h)); }
            else if old.rgba != new.rgba {
                let n = old.rgba.chunks_exact(4).zip(new.rgba.chunks_exact(4)).filter(|(a, b)| a != b).count();
                diffs.push(format!("{name}: {n} pixels differ"));
            }
        }
        assert!(checked >= 100, "only {checked} textures were comparable");
        assert!(diffs.is_empty(), "{} of {checked} textures differ: {:?}", diffs.len(), diffs);
    }
    /// Render `scene` at `w`x`h` with the CPU reference rasteriser over `ctx` (premultiplied RGBA8).
    fn render(ctx: &mut crate::scene::UiContext, scene: &crate::scene::SceneDef, w: u32, h: u32) -> Vec<u8> {
        let list = (scene.build)(ctx, w as f32, h as f32);
        crate::raster::Rasterizer::new().render(&list, &mut ctx.assets, w, h, None)
    }
    fn contexts() -> Option<(crate::scene::UiContext, crate::scene::UiContext)> {
        install()?;
        if !DirAssets::default_root().is_dir() { eprintln!("skipping: no exported folder to compare with"); return None; }
        let dir = crate::scene::UiContext::from_default_assets().ok()?;
        let pak = crate::scene::UiContext::new(Box::new(PakAssets::open(default_install()).ok()?)).expect("fonts from the paks");
        Some((dir, pak))
    }
    #[test] fn settings_video_tab_from_the_pak_equals_the_directory_source_pixel_for_pixel() {
        let Some((mut dir, mut pak)) = contexts() else { return };
        let scene = crate::scene::find_scene("settings18-video").expect("video scene");
        for (w, h) in [(1280, 720), (1920, 1080)] {
            let (a, b) = (render(&mut dir, &scene, w, h), render(&mut pak, &scene, w, h));
            assert_eq!(a.len(), (w * h * 4) as usize);
            assert!(a.chunks_exact(4).any(|p| p[3] != 0), "the scene drew something");
            let diff = a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(x, y)| x != y).count();
            assert_eq!(diff, 0, "{w}x{h}: {diff} pixels differ between the directory source and the pak source");
        }
        assert_eq!(dir.assets.missing, pak.assets.missing, "both sources miss the same optional textures");
    }
    /// Every registered scene (menu, settings tabs, HUD, loading) renders identically from either source.
    #[test] fn every_scene_renders_identically_from_the_pak_and_the_exported_folder() {
        let Some((mut dir, mut pak)) = contexts() else { return };
        let (w, h) = (1280u32, 720u32);
        let mut bad = Vec::new();
        let mut layered = crate::scene::UiContext::new(Box::new(crate::assets::LayeredAssets::new(vec![
            Box::new(PakAssets::open(default_install()).unwrap()), Box::new(DirAssets::new(DirAssets::default_root()))]))).unwrap();
        let (mut menu_scenes, mut loading_scenes) = (0, 0);
        for scene in crate::scene::all_scenes() {
            // Scenes whose art exists only in the paks (Keybase-Center-L, Results_TimerFill / Results_TimerBacking are not in the old exported folder): the folder
            // cannot draw them, so they are compared through the layered source (paks first) against the pak-only source.
            // The picker's T_BattleRoyaleKeyArt.png is pak-only as well.
            let pak_only_art = ["interaction-prompt", "equip-progress-25", "equip-progress-75", "settings18-livestream"].contains(&scene.name) || scene.name.starts_with("picker") || scene.name.starts_with("startup");
            let a = if pak_only_art { render(&mut layered, &scene, w, h) } else { render(&mut dir, &scene, w, h) };
            // The loading screen's tips are localisation text (`tips.txt` of the old prototype, from Game.locres), not a file of the paks: that scene
            // is compared through the layered source (textures from the paks, tips from the folder).
            let loading = scene.name.starts_with("loading") || scene.name.starts_with("startup"); // startup-* are the same loading screen with the tips from the folder
            let b = if loading { render(&mut layered, &scene, w, h) } else { render(&mut pak, &scene, w, h) };
            if loading { loading_scenes += 1 } else { menu_scenes += 1 }
            let diff = a.chunks_exact(4).zip(b.chunks_exact(4)).filter(|(x, y)| x != y).count();
            if diff != 0 { bad.push(format!("{}: {diff} pixels", scene.name)); }
        }
        assert!(bad.is_empty(), "{bad:?}");
        assert!(menu_scenes > 40 && loading_scenes == 8, "{menu_scenes} {loading_scenes}");
        // the paks did serve the loading textures: the only thing the pak-only context lacks is the tips file
        let loading = crate::scene::find_scene("loading").unwrap();
        render(&mut pak, &loading, w, h);
        assert!(pak.assets.missing.iter().all(|m| m.contains("tips.txt")), "{:?}", pak.assets.missing);
    }
    #[test] fn the_pak_source_with_the_exported_folder_as_fallback_serves_both() {
        let Some(_) = install() else { return };
        let root = DirAssets::default_root();
        if !root.is_dir() { return; }
        let layered = crate::assets::LayeredAssets::new(vec![Box::new(PakAssets::open(default_install()).unwrap()), Box::new(DirAssets::new(root))]);
        let mut ua = UiAssets::new(Box::new(layered));
        assert!(ua.texture("Button-Default-M-_normal_.png").id != TextureId::NONE, "from the paks");
        // an exported-only file (the layout of the exported tree has more than the UI textures) is still found
        let only_dir = DirAssets::default_root().join("fonts").join("Content").join("Slate").join("Fonts").join("Roboto-Bold.ttf");
        assert!(only_dir.exists() && ua.source.read("Roboto-Bold.ttf").is_some());
        assert!(ua.texture("not-a-texture.png").id == TextureId::NONE);
    }
    #[test] fn ui_assets_over_the_pak_register_textures_and_record_missing_ones() {
        let Some(_) = install() else { return };
        let dir = default_install();
        let mut ua = UiAssets::new(Box::new(PakAssets::open(&dir).unwrap()));
        let t = ua.texture("Button-Default-M-_normal_.png");
        assert!(t.id != TextureId::NONE && t.w > 4 && t.h > 4, "{t:?}");
        assert_eq!(ua.texture("button-default-m-_normal_.PNG").id, t.id, "cached under a case-insensitive name");
        assert_eq!(ua.texture("not-a-texture.png").id, TextureId::NONE);
        assert_eq!(ua.missing.len(), 1, "{:?}", ua.missing);
    }
}
