//! `fn-manifest`: writes the `asset-manifest.json` that `fn-export` reads (and that `fn-app` looks for beside the
//! executable, through `--manifest` or `FN_MANIFEST`), derived from a Fortnite install's paks.
//!
//! The manifest is the list of packages `fn-export` turns into the runtime's asset tree, so this tool builds it from
//! the runtime's own asset lists instead of a hand-written file:
//!
//! | section      | source                                                                                     |
//! |--------------|--------------------------------------------------------------------------------------------|
//! | `textures`   | `crates/fn-ui/data/ui-texture-map.tsv` (the UI's file name -> package table, incl. the ground texture) |
//! | `aliases`    | (empty: the UI table already resolves the 1.10 `-64` -> `-XL` names)                        |
//! | `meshes`     | the character/skeleton packages `fn-pose::loader` reads (`BODY_PKG`/`HEAD_PKG`/`MASTER_PKG`/`PICK_PKG`); the sky meshes are exported regardless |
//! | `animations` | `fn_legacy::prototype_clip_names()` (the clip set the loader asks for) + the support clips `fn-app` requires |
//!
//! Every entry is resolved against the mounted install, so the manifest only names packages that are really there:
//! `fn-export` exits non-zero for a missing package, which would abort the game's first start-up export.
//!
//! ```text
//! usage: fn-manifest --paks <Paks dir | install dir> [--out tools/asset-manifest.json] [--ui-map crates/fn-ui/data/ui-texture-map.tsv]
//! ```

use fn_pak::{game_path_to_pak, pak_path_to_game, PakSet};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

/// The UI's file name (lower case) -> package table, as `fn-ui::pak_assets` embeds it.
const DEFAULT_UI_MAP: &str = "crates/fn-ui/data/ui-texture-map.tsv";

/// Textures the runtime loads by file name that the UI table does not list (the ground texture `T_Grasslands_D.png`
/// is already in the table, so this is empty; it exists for any future non-UI texture).
const EXTRA_TEXTURES: &[&str] = &[];

/// The sky pass reads these from `<assets>/sky` (`fn_render::sky::TEXTURES`, which also carries the sRGB flag), so their
/// `out` keeps the `sky/` prefix and the package path. Keep in sync with `fn_render::sky::TEXTURES`.
const SKY_TEXTURES: &[&str] = &[
    "Game/Packages/Fortress_Sky/Textures/Galaxy_Background",
    "Game/Packages/Fortress_Sky/Textures/Galaxy_Background_Tiling",
    "Game/VisualThreatManager/StormVisuals/LargeClouds/T_StormCloudDetail01_M",
    "Game/Packages/Fortress_Sky/Textures/T_Sky_Stars_Sharp",
    "Game/Packages/Fortress_Sky/Materials/Noise32x32",
    "Game/Packages/Fortress_Sky/Materials/FinalCloudNormalAndHeightMap",
];

/// The character/skeleton packages the Dir-mode loaders read by name (`fn-pose::loader`).
const MESH_PKGS: &[&str] = &[
    fn_pose::loader::MASTER_PKG,
    fn_pose::loader::BODY_PKG,
    fn_pose::loader::HEAD_PKG,
    fn_pose::loader::PICK_PKG,
];

/// Support clips `fn-app` requires (`assets::REQUIRED`) that are not part of the prototype's clip list.
const EXTRA_ANIMATIONS: &[&str] = &["M_Melee_TwoHanded"];

struct Args {
    paks: PathBuf,
    out: PathBuf,
    ui_map: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { paks: PathBuf::new(), out: PathBuf::from("tools/asset-manifest.json"), ui_map: PathBuf::from(DEFAULT_UI_MAP) };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match x.as_str() {
            "--paks" => a.paks = PathBuf::from(val("--paks")?),
            "--out" => a.out = PathBuf::from(val("--out")?),
            "--ui-map" => a.ui_map = PathBuf::from(val("--ui-map")?),
            "-h" | "--help" => return Err("usage: fn-manifest --paks <Paks dir | install dir> [--out tools/asset-manifest.json] [--ui-map crates/fn-ui/data/ui-texture-map.tsv]".into()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if a.paks.as_os_str().is_empty() {
        return Err("--paks is required".into());
    }
    Ok(a)
}

/// Package stem (lower case) -> `Game/...` paths of its `.uasset`, from one pass over the winning entries.
///
/// The runtime finds a clip with `set.glob("fortnitegame/content/**/<name>.uasset")` and takes its first hit, which
/// depends on the pak-set hash-map order; this map instead keeps every candidate so the caller can pick deterministically
/// and report the ambiguous names.
struct StemIndex(HashMap<String, Vec<String>>);

impl StemIndex {
    fn build(set: &PakSet) -> StemIndex {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for (_, e) in set.effective() {
            let p: &str = e.rel_path();
            let Some(stem) = p.strip_suffix(".uasset") else { continue };
            let low = p.to_ascii_lowercase();
            if !low.starts_with("fortnitegame/content/") && !low.starts_with("engine/content/") {
                continue;
            }
            let name = stem.rsplit('/').next().unwrap_or(stem).to_ascii_lowercase();
            if let Some(game) = pak_path_to_game(p) {
                let game = game.trim_start_matches('/').to_owned();
                let v = m.entry(name).or_default();
                if !v.contains(&game) {
                    v.push(game);
                }
            }
        }
        StemIndex(m)
    }

    /// Every `Game/...` package whose asset name is `stem` (lower case), in the order the runtime's clip loader would
    /// see them (`PakSet::glob` sorts the winning entries by lower-cased path, and the loader takes the first).
    fn candidates(&self, stem: &str) -> Vec<&str> {
        let mut v: Vec<&str> = self.0.get(&stem.to_ascii_lowercase()).map(|v| v.iter().map(String::as_str).collect()).unwrap_or_default();
        v.sort_by_key(|p| p.to_ascii_lowercase());
        v
    }
}

/// Does the install hold the package `pkg` (`Game/...`)?
fn present(set: &PakSet, pkg: &str) -> bool {
    game_path_to_pak(&format!("/{pkg}"), "uasset").map_or(false, |p| set.contains(&p))
}

fn dir_of(pkg: &str) -> &str {
    pkg.rsplit_once('/').map_or("", |(d, _)| d)
}

/// The UI table: `file name<TAB>package`, `#` comments skipped.
fn ui_entries(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| l.split_once('\t'))
        .map(|(n, p)| (n.trim().to_ascii_lowercase(), p.trim().to_owned()))
        .collect()
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let set = PakSet::open_install(&args.paks).map_err(|e| format!("cannot open the paks at {}: {e}", args.paks.display()))?;
    println!("opened {} paks, {} entries", set.paks().len(), set.len());
    let index = StemIndex::build(&set);

    let mut textures: Vec<Value> = Vec::new();
    let mut animations: Vec<Value> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut ambiguous: Vec<String> = Vec::new();

    // 1. UI textures: the file name the UI asks for is also the exported file name (lookups are by file name), so the
    //    output keeps the requested name even where the package stem differs (`-64` -> `-XL`).
    let map_text = std::fs::read_to_string(&args.ui_map).map_err(|e| format!("{}: {e}", args.ui_map.display()))?;
    let mut ui = ui_entries(&map_text);
    ui.sort();
    for (name, pkg) in &ui {
        if present(&set, pkg) {
            textures.push(json!({ "pkg": pkg, "out": format!("exported/{}/{}", dir_of(pkg), name) }));
        } else {
            missing.push(format!("{name} <- {pkg}"));
        }
    }

    // 2. the textures the runtime loads that the UI table does not list (the ground texture for the world).
    for stem in EXTRA_TEXTURES {
        match index.candidates(stem).first() {
            Some(pkg) => textures.push(json!({ "pkg": pkg, "out": format!("exported/{}/{stem}.png", dir_of(pkg)) })),
            None => missing.push(format!("{stem} <- <not found in the paks>")),
        }
    }

    // 3. the sky textures (`<assets>/sky/<package>.png`); the sky meshes themselves need no entry.
    for pkg in SKY_TEXTURES {
        if present(&set, pkg) {
            textures.push(json!({ "pkg": pkg, "out": format!("sky/{pkg}.png") }));
        } else {
            missing.push(format!("{pkg} <- <not found in the paks>"));
        }
    }

    // 4. meshes: the character packages (names match the files the loaders ask for); the sky meshes need no entry.
    let meshes: Vec<Value> = MESH_PKGS
        .iter()
        .map(|p| p.trim_start_matches('/'))
        .filter(|p| {
            if present(&set, p) {
                true
            } else {
                missing.push(format!("{p} <- <not found in the paks>"));
                false
            }
        })
        .map(|p| json!(p))
        .collect();

    // 5. animations: the clip set the loader reads by name, resolved to its package.
    let mut clips: Vec<String> = fn_legacy::prototype_clip_names();
    for n in EXTRA_ANIMATIONS {
        if !clips.iter().any(|c| c == n as &str) {
            clips.push((*n).to_owned());
        }
    }
    clips.sort();
    clips.dedup();
    for name in &clips {
        let c = index.candidates(name);
        let Some(pkg) = c.first() else {
            missing.push(format!("{name} <- <not found in the paks>"));
            continue;
        };
        if c.len() > 1 {
            ambiguous.push(format!("{name}: {}", c.join(", ")));
        }
        animations.push(json!({ "pkg": pkg, "out": format!("exported/{pkg}.md5anim") }));
    }

    let manifest = json!({ "textures": textures, "aliases": [], "meshes": meshes, "animations": animations });
    if let Some(p) = args.out.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    std::fs::write(&args.out, serde_json::to_string_pretty(&manifest).unwrap() + "\n").map_err(|e| format!("{}: {e}", args.out.display()))?;

    println!(
        "wrote {}: {} textures, {} meshes, {} animations",
        args.out.display(),
        manifest["textures"].as_array().map_or(0, Vec::len),
        manifest["meshes"].as_array().map_or(0, Vec::len),
        manifest["animations"].as_array().map_or(0, Vec::len),
    );
    if !ambiguous.is_empty() {
        println!("ambiguous names (more than one package; the one the runtime's glob picks first was taken):");
        for a in &ambiguous {
            println!("  {a}");
        }
    }
    if !missing.is_empty() {
        println!("not in this install ({}):", missing.len());
        for m in &missing {
            println!("  {m}");
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("fn-manifest: {e}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_table_ignores_comments_and_keeps_lower_case_names() {
        let t = "# header\nAliveIco.PNG\tGame/Athena/HUD/Art/AliveIco\n\nallblack.png\tGame/Characters/Fortress_Character/Textures/AllBlack\n";
        assert_eq!(
            ui_entries(t),
            vec![
                ("aliveico.png".to_owned(), "Game/Athena/HUD/Art/AliveIco".to_owned()),
                ("allblack.png".to_owned(), "Game/Characters/Fortress_Character/Textures/AllBlack".to_owned()),
            ]
        );
    }

    #[test]
    fn dir_of_splits_the_package_directory() {
        assert_eq!(dir_of("Game/A/B/C"), "Game/A/B");
        assert_eq!(dir_of("C"), "");
    }

    #[test]
    fn embedded_ui_table_parses() {
        let entries = ui_entries(include_str!("../../fn-ui/data/ui-texture-map.tsv"));
        assert!(entries.len() > 150, "{} entries", entries.len());
        assert!(entries.iter().all(|(n, p)| n.ends_with(".png") && (p.starts_with("Game/") || p.starts_with("Engine/"))));
    }

    #[test]
    fn mesh_package_consts_are_unique_and_long() {
        let mut v: Vec<&str> = MESH_PKGS.to_vec();
        v.sort();
        v.dedup();
        assert_eq!(v.len(), MESH_PKGS.len());
        assert!(MESH_PKGS.iter().all(|p| p.starts_with("/Game/")));
    }
}
