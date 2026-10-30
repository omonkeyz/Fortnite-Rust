//! `fortnite-rs`: the pure-Rust game binary. See `README.md` of this crate for the architecture and `cli.rs` for the command line.
//!
//! Release builds are GUI-subsystem executables (no console window on double click); `diag::init` attaches to the parent console when started from a
//! terminal, tees stderr into `fortnite-rs.log` beside the exe and installs the panic hook. Debug builds stay console-subsystem.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod ads; // ADS HOOK
mod weapon_runtime; // WEAPON RUNTIME: any BR firearm, lazily loaded
mod app;
mod arms;
mod assets;
mod audio;
mod backdrop_load; // BACKDROP HOOK
mod backdrop_real; // BACKDROP REAL: the far skybox terraces / rocks on their real cooked material (backdrop pass)
mod cam_pose; // UE POSE CAMERA (dev): FN_CAM_UE=X,Y,Z,PITCH,YAW[,FOV]
mod backend;
mod br_character; // BR CHARACTER HOOK
mod build_input; // BUILD INPUT HOOK: build mode / pieces / material / edit keys
mod build_anim; // BUILD ANIM HOOK: building blueprint + pencil in the hands, PaperBlueprint set, BuildingState poses, build / place / edit montages
mod building_edit_hook; // BUILDING EDIT HOOK: fn_building::edit (edit mode, tiles, confirm/reset)
mod building_render; // BUILDING RENDER HOOK: placed pieces, build-up / damage material animation, blueprint ghost, preview, edit tiles, building FX + sounds
mod frontend_bg; // FRONT END BACKDROP: the 3D front-end level behind the login screen / picker (Frontend_BG_Main, login camera, renderer front-end mode)
mod landscape_render; // LANDSCAPE MATERIAL: the main-pass landscape on its real cooked material (FX path, landscape vertex factory)
mod statics_render; // STATIC MATERIALS: layout props drawn with their real cooked material shaders (FX scene-lit path)
mod spline_mesh; // ROADS / SPLINES: the landscape splines' SplineMeshComponents (the 1.8 streams) with UE's spline deformation (spline_mesh.rs, FN_ROADS=0 = off)
#[allow(dead_code)] // called by the owner of the app's BuildingWorld (building core hook) once it exists
mod building_hook; // BUILDING HOOK (core): the BuildingWorld, preview, placing, build-up, piece collision, PLACEHOLDER piece visuals
mod building_damage_hook; // BUILDING DAMAGE HOOK: pickaxe / bullets -> fn_building::damage, structural collapse, destroyed-piece events
mod building_hud; // BUILDING HUD: the targeted building (pieces, the build preview, props) for Target Info + the pickaxe reticle colour
mod cli;
mod console_hook; // CONSOLE HOOK: F2 command bar (front-end half of fn-console)
mod console_ui; // CONSOLE HOOK: its overlay
mod df_hook; // DISTANCE FIELD HOOK: cooked mesh distance fields -> renderer
mod diag;
mod foliage; // FOLIAGE HOOK
mod grass_render; // GRASS: the ground cover with its real cooked material (FInstancedStaticMeshVertexFactory)
mod gfx;
mod harvest_fx; // HARVEST HOOK
mod hitreg_hook; // HITREG HOOK
mod input;
mod inventory; // WIDGETS3 HOOK
mod keys;
mod loader;
mod gun_orig; // GUN MATERIAL HOOK
#[cfg(test)] mod gun_fit; // GUN FIT: held-gun fit measurement
#[cfg(test)] mod hand_fit; // HAND FIT: left-hand-off-gun measurement + sway
#[cfg(test)] mod aim_pitch; // AIM PITCH / RELAXED: held-gun pitch measurement
mod loadout_preview;
mod lobby_world; // actual Frontend_Lobby_Halloween static actor extraction
mod lobby_scene; // FRONT END BACKDROP: the front-end scene renderer (frontend_bg.rs draws the Login camera view with it)
mod lobby_profile;
mod lobby_locker;
mod lobby_shop;
mod lobby_ui;
mod lobby_mm;
mod lobby_host;
mod lobby_party;
mod lobby_fx;
mod login_name;
mod name_store;
mod chat_hook;
mod pad;
mod paths; // self-locating assets / paks (replaces the old launcher .cmd)
mod part_anim_glue; // PART ANIM HOOK
mod muzzle_hook; // MUZZLE HOOK
mod foot_fx; // FOOT FX: walk / run / land dust (foot_fx.rs)
mod heal_fx; // HEAL FX: P_Consume_Item burst when a healing item applies (heal_fx.rs)
mod water_hook;
mod waterfall_fx;
mod hurt_hook; // HURT HOOK: fall damage -> shield / health, damage direction lens effect, damage camera shake (hurt_hook.rs)
mod kill_feed_hook;
mod death_hook;
mod dbno_hook;
mod emote_hook;
mod mpc_hook; // MATERIAL PARAMETERS: the world's FortniteMaterialParameters instance over the shared collection buffer
mod scope_hook; // SCOPE HOOK: the scoped weapon's post-process material + scope mesh
mod decals_hook;
mod outline_hook; // OUTLINE HOOK: the build preview highlight of blocking / destroyed props
mod particles; // PARTICLE HOOK
mod pickup_mesh; // PICKUPS HOOK: the displayed mesh of a world item
mod pickup_orig; // PICKUP ITEM LIGHTING: weapon items with their original material in the gun pass (as the held gun)
mod local_lights_hook;
mod light_fn_hook; // LIGHT FUNCTION HOOK: the sun light function textures + world offset (fn-render light_function.rs)
mod sky_refl_hook; // SKY REFLECTION HOOK: T_hdrsky_test03_low filtered like FilterReflectionEnvironment -> the renderer's sky specular
mod pickups_fx; // PICKUPS FX: rarity effects of the world items
mod pickups_dxbc; // PICKUPS FX: the original cooked shader programs of their materials
mod pickups_hook; // PICKUPS HOOK: world items (AFortPickup): spawn / toss / pick up / drop
mod interact; // INTERACT (2026-10-08): shared Use-key state for interactable hooks (doors, containers, ...)
mod mp_hook;
mod bots_hook;
mod door_hook; // DOOR HOOK (2026-10-08): map door walls + player door edits (fn_building::door): Use, swing, collision, sounds, draws
mod trap_hook;
mod bounce_hook;
mod rocket_hook;
mod gunfx_hook;
mod bus_hook;
mod elim_fx;
mod skydive_anim;
mod enhanced_anim_load;
mod enhanced_sound;
mod demolition_hook;
mod floor_loot_hook;
mod supply_drop_hook;
mod chest_hook; // CHEST HOOK (2026-10-08): BR chests / ammo boxes: closed FX + ambient loop, hold-to-search, open burst, loot tossed as pickups (fn_game::containers)
mod platform;
mod poi; // POI (dev): `--world-center` + the town static meshes of `fn-export props poi`
mod props;
mod run;
mod scene;
mod script;
mod settings_guard;
mod stats;
mod storm_hook; // STORM HOOK: safe-zone wall + storm clouds
mod sys;
mod ui;
// BROWSER PORT: the wasm entry + page glue of the browser build (tools/web/build.sh compiles this module tree as the library of tools/web/fn-web)
#[cfg(target_arch = "wasm32")]
#[path = "../../../tools/web/fn-web/src/web.rs"]
mod web;

use assets::AssetRoots;
use cli::Options;
use loader::Source;
use std::path::PathBuf;
use std::process::ExitCode;

/// Files needed in pak mode (everything else comes out of the paks).
const PAK_MODE_REQUIRED: &[&str] = &["T_Grasslands_D.png", "dome_optimized.bin", "sunmoon.bin"];

/// Why start-up cannot continue, and how it is reported.
#[derive(Debug)]
enum Startup {
    /// Required game files cannot be found: native box naming the files and every place that was searched, exit code 1.
    MissingAssets(String),
    /// Anything else (bad --assets / --paks, pak open failure, export failure): same box, exit code 1.
    Other(String),
}
impl Startup { fn text(&self) -> &str { match self { Startup::MissingAssets(s) | Startup::Other(s) => s } } }

fn missing_in(roots: &AssetRoots, names: &[&'static str]) -> Vec<&'static str> { names.iter().copied().filter(|n| roots.find(n).is_none()).collect() }

/// The text of the missing-assets message: the file names, the searched places, what to do.
fn missing_assets_text(missing: &[&str], search: &str) -> String {
    format!("The game cannot start: required game files are missing or incomplete.\n\nMissing: {}\n\n{search}\nKeep fortnite-rs.exe next to its `assets` folder, or start it with --assets <folder> (or set FN_ASSETS), or with --paks <Paks folder> (or set FN_ROOT / FN_PAKS) to read the files from a Fortnite 1.8 install.", missing.join(", "))
}

/// Decides where meshes / animations come from and which asset roots are searched.
fn resolve(opts: &Options) -> Result<(AssetRoots, Source), Startup> {
    let mut roots = AssetRoots::open(assets::candidate_roots(opts.assets.as_deref()));
    if let (Some(a), true) = (&opts.assets, roots.roots.is_empty()) { return Err(Startup::Other(format!("--assets {} is not a directory", a.display()))); }
    // an explicit --paks always means "meshes and animations from the paks"
    let explicit = match &opts.paks {
        Some(p) => Some(assets::find_paks_dir(p).ok_or_else(|| Startup::Other(format!("--paks {}: no .pak files found there (give the Paks folder or the install folder)", p.display())))?),
        None => None,
    };
    let dir_missing = roots.missing_required();
    let paks_dir = explicit.clone().or_else(|| if dir_missing.is_empty() { None } else { assets::candidate_paks(None).iter().find_map(|p| assets::find_paks_dir(p)) });
    let Some(paks_dir) = paks_dir else {
        if dir_missing.is_empty() { return Ok((roots, Source::Dir)); }
        return Err(Startup::MissingAssets(missing_assets_text(&dir_missing, &assets::describe_search(opts.assets.as_deref(), opts.paks.as_deref()))));
    };
    let (n_paks, n_entries) = assets::probe_paks(&paks_dir).map_err(Startup::Other)?;
    eprintln!("[paks] {} : {n_paks} pak files, {n_entries} entries", paks_dir.display());
    let still_missing = missing_in(&roots, PAK_MODE_REQUIRED);
    if !still_missing.is_empty() {
        // textures / sky meshes are not decoded in-process: let fn-export produce them into a cache that is searched first
        let cache = assets::exe_dir().join("assets-cache");
        let manifest = assets::find_manifest(opts.manifest.as_deref()).ok_or_else(|| Startup::MissingAssets(missing_assets_text(&still_missing, &format!("No asset-manifest.json was found for fn-export (use --manifest, FN_MANIFEST, or put it beside the exe), so the files cannot be exported from the paks.\n{}", assets::describe_search(opts.assets.as_deref(), opts.paks.as_deref())))))?;
        eprintln!("[paks] exporting the missing files with fn-export into {} ...", cache.display());
        assets::export_from_paks(&paks_dir, &manifest, &cache, |l| eprintln!("[fn-export] {l}")).map_err(Startup::Other)?;
        let mut r = vec![cache]; r.extend(roots.roots.iter().cloned());
        roots = AssetRoots::open(r);
        let still = missing_in(&roots, PAK_MODE_REQUIRED);
        if !still.is_empty() { return Err(Startup::MissingAssets(missing_assets_text(&still, "The export from the paks finished but these files are still not there.\n"))); }
    }
    Ok((roots, Source::Paks(paks_dir)))
}

fn out_dir(opts: &Options) -> PathBuf { opts.out.clone().unwrap_or_else(|| assets::exe_dir().join("verification")) }

/// Validates the numeric options of a run (the parser accepts any number): `--frames 0` ran one frame and reported success, `--size 100x100` was
/// silently raised to the minimum window size.
fn check_options(opts: &Options) -> Result<(), String> {
    if opts.frames == Some(0) { return Err("--frames must be at least 1 (0 would run nothing)".into()); }
    if let Some((w, h)) = opts.size { platform::check_client_size(w, h, "--size")?; }
    Ok(())
}

fn build_run(opts: &Options) -> Result<Option<run::ScriptRun>, String> {
    let out = out_dir(opts);
    if let Some(path) = &opts.screenshot {
        let (w, h) = opts.size.unwrap_or((1280, 720));
        let text = format!("size {w}x{h}\nfps 60\nprops real\nend 31\n30 shot screenshot\n");
        let d: Box<dyn script::Driver> = Box::new(script::Script::parse("screenshot", &text)?);
        let mut r = run::ScriptRun::new("screenshot", d, path.parent().map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| PathBuf::from(".")), opts.frames);
        r.shot_override = Some(path.clone());
        return Ok(Some(r));
    }
    let spec = if opts.smoke { Some("builtin:smoke".to_owned()) } else { opts.script.clone() };
    match spec {
        Some(spec) => { let d = script::load(&spec)?; Ok(Some(run::ScriptRun::new(&spec, d, out, opts.frames))) }
        None => Ok(None),
    }
}

/// Reads the user's settings file once before the app does ([`settings_guard`]): prints / logs what was repaired or moved aside (a corrupt
/// `FortniteSettings.ini` is moved to `.corrupt` and defaults are used; that used to happen silently) and writes a repaired `Video.Resolution` back so a
/// bad value is fixed for good. The app's own load afterwards sees the already-clean file.
fn check_settings_file(opts: &Options) {
    if !opts.uses_user_settings() { return; }
    let checked = settings_guard::check_and_repair(&fn_settings::default_path());
    if let Some(note) = &checked.note { eprintln!("[settings] {note}"); }
}

fn fail_startup(opts: Option<&Options>, e: &Startup, code: u8) -> ExitCode {
    eprintln!("error: {}", e.text());
    if opts.map_or(true, |o| !o.scripted() && !o.live_test) { diag::show_error(diag::TITLE, e.text()); }
    ExitCode::from(code)
}

fn main() -> ExitCode {
    diag::init();
    let code = run_main();
    diag::flush(); // stderr goes through the tee pipe: make sure the last lines are out before the process ends
    code
}

fn run_main() -> ExitCode {
    // Diagnostic hook for the panic path (hook output, native box, exit code 101): `FN_TEST_PANIC=1 fortnite-rs`.
    if fn_live::var_os("FN_TEST_PANIC").is_some() { panic!("FN_TEST_PANIC is set: deliberate test panic"); }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = audio::strip_cli(args); // AUDIO HOOK 1/2: --no-audio / --audio / --audio-test / --audio-device-smoke are the audio module's, not cli.rs's
    if let Some(code) = audio::run_mode(&args) { return code; } // AUDIO HOOK 2/2: the window-less audio modes
    let opts = match cli::parse(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}\n\n{}", cli::USAGE);
            if !diag::stderr_visible() { diag::show_error(diag::TITLE, &format!("{e}\n\n{}", cli::USAGE)); } // double-clicked shortcut with bad arguments: nobody sees stderr
            return ExitCode::from(2);
        }
    };
    // Automated runs (scripts, screenshots, the live self-test) never open modal boxes: a harness must not hang on a dialog.
    diag::set_dialogs(!opts.scripted() && !opts.live_test);
    if opts.help { println!("fortnite-rs {} (Fortnite {} standalone runtime)\n\n{}", env!("CARGO_PKG_VERSION"), fn_settings::version::FORTNITE_VERSION, format!("{}\n{}", cli::USAGE, audio::USAGE)); return ExitCode::SUCCESS; }
    // `--assets` / `--paks` absent: FortniteRuntime.toml / paths.txt beside the exe, then (interactive launches only) the known default locations (paths.rs)
    let opts = paths::apply_defaults(opts, &assets::exe_dir(), &paths::Known::developer());
    if let Err(e) = check_options(&opts) { eprintln!("error: {e}"); return ExitCode::from(2); }
    let run = match build_run(&opts) { Ok(r) => r, Err(e) => { eprintln!("error: {e}"); return ExitCode::from(2); } };
    let (roots, source) = match resolve(&opts) { Ok(x) => x, Err(e) => return fail_startup(Some(&opts), &e, 1) };
    eprintln!("[assets] roots: {:?}  source: {}", roots.roots, match &source { Source::Dir => "asset tree (glTF + md5anim)".to_owned(), Source::Paks(p) => format!("paks {}", p.display()) });
    check_settings_file(&opts);
    let event_loop = match winit::event_loop::EventLoop::new() { Ok(e) => e, Err(e) => return fail_startup(Some(&opts), &Startup::Other(format!("cannot start the event loop: {e}")), 1) };
    let mut app = app::App::new(opts, roots, source, run);
    if let Err(e) = event_loop.run_app(&mut app) { return fail_startup(Some(&app.opts), &Startup::Other(e.to_string()), 1); }
    if app.exit_code != 0 && diag::dialogs_enabled() && !matches!(app.phase(), app::Phase::Failed(_)) {
        // the app loop ended with a failure before an in-window error page existed (window or GPU creation failed): say why
        if let Some(why) = diag::last_error() { diag::show_error(diag::TITLE, &why); }
    }
    ExitCode::from(app.exit_code as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn a(s: &str) -> Vec<String> { s.split_whitespace().map(String::from).collect() }
    #[test] fn frames_zero_and_out_of_range_sizes_are_rejected_not_silently_fixed() {
        assert!(check_options(&cli::parse(&a("--smoke-test --frames 0")).unwrap()).unwrap_err().contains("--frames"));
        assert!(check_options(&cli::parse(&a("--smoke-test --frames 1")).unwrap()).is_ok());
        let e = check_options(&cli::parse(&a("--screenshot x.png --size 100x100")).unwrap()).unwrap_err(); assert!(e.contains("100x100") && e.contains("640x360"), "{e}");
        assert!(check_options(&cli::parse(&a("--screenshot x.png --size 640x359")).unwrap()).is_err());
        assert!(check_options(&cli::parse(&a("--screenshot x.png --size 9000x720")).unwrap()).unwrap_err().contains("8192"));
        assert!(check_options(&cli::parse(&a("--screenshot x.png --size 640x360")).unwrap()).is_ok());
        assert!(check_options(&cli::parse(&a("--screenshot x.png --size 8192x8192")).unwrap()).is_ok());
    }
    #[test] fn the_missing_assets_text_names_the_files_and_the_places() {
        let t = missing_assets_text(&["SK_Pick.gltf", "sunmoon.bin"], "Looked for the asset folder in:\n  beside the executable: C:/g/assets (not found)\n");
        assert!(t.contains("SK_Pick.gltf, sunmoon.bin") && t.contains("C:/g/assets") && t.contains("--assets") && t.contains("FN_ASSETS") && t.contains("--paks"), "{t}");
    }
    #[test] fn nothing_found_anywhere_is_a_missing_assets_error_that_lists_the_search() {
        let tmp = std::env::temp_dir().join(format!("fn-app-nothing-{}", std::process::id())); let _ = std::fs::remove_dir_all(&tmp); std::fs::create_dir_all(&tmp).unwrap();
        // an explicit but EMPTY assets folder: no fallback to anything baked in may rescue it
        let opts = Options { assets: Some(tmp.clone()), ..Default::default() };
        match resolve(&opts) {
            Err(Startup::MissingAssets(t)) => { assert!(t.contains("SK_M_Med_Soldier_04.gltf") && t.contains("--assets") && t.contains(&*tmp.to_string_lossy().replace('\\', "\\")), "{t}"); }
            Err(Startup::Other(t)) => panic!("unexpected: {t}"),
            Ok(_) => panic!("must not find game files in an empty folder (is a developer fallback still in place?)"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

