//! `fn-vfx-export`: reads the muzzle flash data of every BR firearm from the 1.8 paks.
//!
//! ```text
//! fn-vfx-export [--paks <Paks dir>] [--out <dir>] [--list]
//! ```
//! For every `/Game/Athena/Items/Weapons/WID_*` ranged weapon: `WeaponActorClass` -> the Blueprint chain (`B_*_Athena` -> ... -> `B_Ranged_Generic`) -> the CDO properties
//! `MuzzleParticleSystem`, `MuzzleSocketName`, `bPersistentFireFX` (first Blueprint in the chain that sets them), every other particle reference of those CDOs, the `*Light*`
//! numbers, and the muzzle socket of the weapon mesh. Then the referenced particle systems (with their materials / textures / meshes) are decoded by `fn-particles` into
//! `<out>` (`index.json`, `systems/`, ...) next to `<out>/muzzle.json`.
use fn_uasset::{Package, PackageData, PackageIndex, Value};
use fn_vfx::table::{MuzzleTable, ReloadSmokeData, ShellsData, SocketInfo, WeaponMuzzle};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const WID_DIR: &str = "FortniteGame/Content/Athena/Items/Weapons/";

fn pkg_of(path: &str) -> String { path.split('.').next().unwrap_or(path).to_string() }
fn strip_class(p: &str) -> String { pkg_of(p) }

/// Hard object reference, or a soft object / soft class path string (`WeaponActorClass`, `WeaponMeshOverride` are soft).
fn object_path(pkg: &Package, v: &Value) -> Option<String> {
    if let Some(i) = v.as_object() { let p = pkg.object_path(i); return if p.is_empty() { None } else { Some(p) }; }
    v.as_str().filter(|s| !s.is_empty()).map(String::from)
}

struct Cdo { props: Vec<(String, Value)>, pkg_particles: BTreeMap<String, String>, objects: BTreeMap<String, String>, super_path: Option<String> }

/// CDO properties of a Blueprint package (+ its resolved super class package, `None` for native supers).
fn load_cdo(set: &fn_pak::PakSet, long: &str) -> Result<Cdo, String> {
    let data = PackageData::load(set, long).map_err(|e| format!("{long}: {e}"))?;
    let pkg = data.package().map_err(|e| format!("{long}: {e}"))?;
    let super_path = pkg.super_class().map(|r| r.path().to_string()).filter(|p| p.starts_with("/Game/")).map(|p| pkg_of(&p));
    let mut props = vec![];
    let mut particles = BTreeMap::new();
    let mut objects = BTreeMap::new();
    if let Some(Ok(cdo)) = pkg.read_default_object() {
        for p in cdo.props.iter() {
            let key = if p.index > 0 { format!("{}[{}]", p.name, p.index) } else { p.name.clone() };
            // particle system references (class of the referenced object = ParticleSystem)
            if let Value::Object(i) = &p.value {
                if let ObjectClass::Particle = object_class(&pkg, *i) { if let Some(path) = object_path(&pkg, &p.value) { particles.insert(key.clone(), pkg_of(&path)); } }
            }
            if let Some(path) = object_path(&pkg, &p.value) { objects.insert(key.clone(), path); }
            props.push((key, p.value.clone()));
        }
    }
    Ok(Cdo { props, pkg_particles: particles, objects, super_path })
}
enum ObjectClass { Particle, Other }
fn object_class(pkg: &Package, i: PackageIndex) -> ObjectClass {
    match pkg.resolve(i) { fn_uasset::ObjectRef::Import { class_name, .. } if class_name == "ParticleSystem" => ObjectClass::Particle, _ => ObjectClass::Other }
}

fn find_socket(set: &fn_pak::PakSet, mesh_long: &str, socket: &str) -> Option<SocketInfo> {
    let load = |long: &str| -> Option<fn_ueio::Package> {
        let ua = set.read_game(long, "uasset").ok()?.into_owned();
        let ue = set.read_game(long, "uexp").map(|c| c.into_owned()).unwrap_or_default();
        fn_ueio::Package::parse(&ua, &ue, &[]).ok()
    };
    let pkg = load(mesh_long)?;
    let sm = fn_mesh::SkeletalMesh::parse(&pkg).ok()?;
    let conv = |s: &fn_mesh::skeleton::Socket, owner: &str| SocketInfo { name: s.name.clone(), bone: s.bone.clone(), location: s.location, rotation: s.rotation, scale: s.scale, owner: owner.into() };
    if let Some(s) = sm.sockets.iter().find(|s| s.name == socket) { return Some(conv(s, "mesh")); }
    let sp = sm.skeleton_path.as_deref()?;
    let spkg = load(sp.split('.').next()?)?;
    let sk = fn_mesh::Skeleton::parse(&spkg).ok()?;
    sk.sockets.iter().find(|s| s.name == socket).map(|s| conv(s, "skeleton"))
}

fn reload_notifies(set: &fn_pak::PakSet, long: &str) -> Vec<(String, f32)> {
    let Ok(data) = PackageData::load(set, long) else { return vec![] };
    let Ok(pkg) = data.package() else { return vec![] };
    let mut stages: BTreeMap<String, String> = BTreeMap::new();
    for i in 0..pkg.exports.len() {
        if !pkg.export_class_name(i).starts_with("AnimNotify_PlayReloadFX") { continue; }
        let Ok(e) = pkg.read_export(i) else { continue };
        stages.insert(pkg.exports[i].name.clone(), e.get("ReloadStage").and_then(|v| v.as_str()).unwrap_or("").to_string());
    }
    let mut out = vec![];
    for i in 0..pkg.exports.len() {
        let Ok(e) = pkg.read_export(i) else { continue };
        let Some(arr) = e.get("Notifies").and_then(|v| v.as_array()) else { continue };
        for n in arr {
            let Some(st) = n.as_struct() else { continue };
            let Some(obj) = st.get("Notify").and_then(|v| v.as_object()) else { continue };
            let path = pkg.object_path(obj);
            let leaf = path.rsplit([':', '.']).next().unwrap_or("").to_string();
            let Some(stage) = stages.get(&leaf) else { continue };
            out.push((stage.clone(), st.get("LinkValue").and_then(|v| v.as_f32()).unwrap_or(0.0) + st.get("TriggerTimeOffset").and_then(|v| v.as_f32()).unwrap_or(0.0)));
        }
    }
    out.sort_by(|a, b| a.1.total_cmp(&b.1));
    out
}

fn pump_notifies(set: &fn_pak::PakSet, long: &str) -> Vec<f32> {
    let Ok(data) = PackageData::load(set, long) else { return vec![] };
    let Ok(pkg) = data.package() else { return vec![] };
    let mut out = vec![];
    for i in 0..pkg.exports.len() {
        let Ok(e) = pkg.read_export(i) else { continue };
        let Some(arr) = e.get("Notifies").and_then(|v| v.as_array()) else { continue };
        for n in arr {
            let Some(st) = n.as_struct() else { continue };
            let Some(obj) = st.get("Notify").and_then(|v| v.as_object()) else { continue };
            let cls = pkg.object_path(obj);
            if !cls.contains("AnimNotify_ShellsOnPumpFX") { continue; }
            let t = st.get("LinkValue").and_then(|v| v.as_f32()).unwrap_or(0.0) + st.get("TriggerTimeOffset").and_then(|v| v.as_f32()).unwrap_or(0.0);
            out.push(t);
        }
    }
    out
}

fn main() {
    let mut paks = PathBuf::from("C:/Users/Brady/Downloads/1.8/Fortnite/Fortnite/FortniteGame/Content/Paks");
    let mut out = PathBuf::from("assets/particles-muzzle");
    let mut list = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--paks" => paks = PathBuf::from(it.next().expect("--paks needs a value")),
            "--out" => out = PathBuf::from(it.next().expect("--out needs a value")),
            "--list" => list = true,
            other => { eprintln!("unknown argument {other}"); std::process::exit(2) }
        }
    }
    let set = match fn_pak::PakSet::open_install(&paks) { Ok(s) => s, Err(e) => { eprintln!("cannot open the paks at {}: {e}", paks.display()); std::process::exit(2) } };
    let mut wids: Vec<String> = set.glob(&format!("{WID_DIR}WID_*.uasset")).into_iter().filter_map(|(_, e)| fn_pak::pak_path_to_game(e.rel_path())).collect();
    wids.sort(); wids.dedup();
    let mut table = MuzzleTable { schema: 1, notes: vec![
        "muzzle_system = MuzzleParticleSystem of the first Blueprint in the chain that sets it (CDO); the Muzzle(Empty) component of B_Ranged_Generic is attached to the weapon mesh socket MuzzleSocketName and uses it as its template (UserConstructionScript)".into(),
        "muzzle_socket falls back to `Muzzle` when no Blueprint sets MuzzleSocketName: INFERENCE (the native default lives in the exe); `socket` is the real socket of the mesh / skeleton with that name".into(),
    ], weapons: BTreeMap::new() };
    let mut systems: BTreeSet<String> = BTreeSet::new();
    for wid in wids {
        let name = wid.rsplit('/').next().unwrap_or("").to_string();
        if name.contains("Harvest") { continue; }
        let Ok(data) = PackageData::load(&set, &wid) else { continue };
        let Ok(pkg) = data.package() else { continue };
        let Some(Ok(def)) = (0..pkg.exports.len()).find(|&i| pkg.export_class_name(i).ends_with("ItemDefinition")).map(|i| pkg.read_export(i)) else { continue };
        if !pkg.export_class_name(0).contains("WeaponRanged") { continue; }
        let Some(actor) = def.get("WeaponActorClass").and_then(|v| object_path(&pkg, v)) else { eprintln!("{name}: no WeaponActorClass"); continue };
        let mesh = def.get("WeaponMeshOverride").and_then(|v| object_path(&pkg, v)).map(|p| pkg_of(&p)).unwrap_or_default();
        let mut w = WeaponMuzzle { wid: wid.clone(), actor_class: strip_class(&actor), mesh: mesh.clone(), muzzle_socket: "Muzzle".into(), ..Default::default() };
        let mut cur = Some(strip_class(&actor)); let mut seen_persistent = false;
        while let Some(bp) = cur.take() {
            if w.chain.len() > 12 { break; }
            let cdo = match load_cdo(&set, &bp) { Ok(c) => c, Err(e) => { eprintln!("{name}: {e}"); break } };
            w.chain.push(bp.clone());
            for (k, v) in &cdo.props {
                match k.as_str() {
                    "MuzzleParticleSystem" if w.muzzle_system.is_none() => { if let Some(p) = cdo.pkg_particles.get(k) { w.muzzle_system = Some(p.clone()); } }
                    "MuzzleSocketName" if !w.socket_from_data => { if let Some(s) = v.as_str() { w.muzzle_socket = s.to_string(); w.socket_from_data = true; } }
                    "bPersistentFireFX" if !seen_persistent => { if let Some(b) = v.as_bool() { w.persistent_fire = b; seen_persistent = true; } }
                    _ => {}
                }
                if k.contains("Light") { if let Some(f) = v.as_f32() { w.light_properties.entry(k.clone()).or_insert(f); } }
            }
            for (k, p) in &cdo.pkg_particles { if k != "MuzzleParticleSystem" && !k.contains("ImpactPhysicalSurface") { w.other_particle_refs.entry(k.clone()).or_insert_with(|| p.clone()); } }
            cur = cdo.super_path;
        }
        let mut first: BTreeMap<String, (Value, String, String)> = BTreeMap::new();
        for bp in &w.chain {
            let Ok(cdo) = load_cdo(&set, bp) else { continue };
            for (k, v) in &cdo.props { if !first.contains_key(k) { first.insert(k.clone(), (v.clone(), cdo.pkg_particles.get(k).cloned().unwrap_or_default(), cdo.objects.get(k).cloned().unwrap_or_default())); } }
        }
        let fb = |k: &str| first.get(k).and_then(|v| v.0.as_bool()).unwrap_or(false);
        let ff = |k: &str, d: f32| first.get(k).and_then(|v| v.0.as_f32()).unwrap_or(d);
        let fv = |k: &str| first.get(k).and_then(|v| v.0.as_vec3()).unwrap_or([0.0; 3]);
        let fs = |k: &str| first.get(k).and_then(|v| v.0.as_str().map(String::from));
        let fp = |k: &str| first.get(k).map(|v| v.1.clone()).filter(|p| !p.is_empty());
        let fo = |k: &str| first.get(k).map(|v| pkg_of(&v.2)).filter(|p| !p.is_empty());
        let (on_fire, on_reload, on_pump) = (fb("UseShellsOnFire?"), fb("UseShellsOnReload?"), fb("UseShellsOnPump?"));
        if on_fire || on_reload || on_pump {
            if let Some(sys) = fp("ShellsParticleSystemTemplate") {
                let socket_name = fs("Shells Socket Name").unwrap_or_default();
                let shell_type = fs("ShellTypeSelect").and_then(|e| e.rsplit("NewEnumerator").next().and_then(|n| n.parse::<u8>().ok())).unwrap_or(0);
                let mut pump_notify_times = vec![];
                if on_pump {
                    let mut ms: Vec<String> = ["WeaponFireMontage", "WeaponFireDownsightsMontage"].iter().filter_map(|k| fo(k)).collect();
                    ms.sort(); ms.dedup();
                    for m in &ms { let t = pump_notifies(&set, m); println!("{name}: {m} ShellsOnPumpFX at {t:?}"); pump_notify_times.extend(t); }
                    pump_notify_times.sort_by(|a, b| a.total_cmp(b)); pump_notify_times.dedup();
                }
                let reload_shell_delay = if on_reload { fo("ReloadAnimation").and_then(|m| { let n = reload_notifies(&set, &m); println!("{name}: {m} PlayReloadFX {n:?}"); let max = n.iter().find(|x| x.0.ends_with("Max_None"))?.1; let start = n.iter().filter(|x| x.0.is_empty() || x.0.ends_with("ReloadStart")).map(|x| x.1).filter(|t| *t <= max).fold(None, |a: Option<f32>, t| Some(a.map_or(t, |a| a.min(t))))?; Some(max - start) }) } else { None };
                let socket = if mesh.is_empty() { None } else { find_socket(&set, &mesh, &socket_name) };
                if socket.is_none() { eprintln!("{name}: shells socket `{socket_name}` not on {mesh}: attached at the mesh origin"); }
                systems.insert(sys.clone());
                w.shells = Some(ShellsData { system: sys, socket_name, socket, on_fire, on_reload, on_pump, shell_type, spawn_rate_scale: ff("Shells Spawn Rate Scale", 1.0), rotation_rate: fv("ShellsRotationRate"), velocity: fv("Shells Velocity"), gravity: fv("Shells Gravity"), lifetime: ff("Shells Lifetime", 1.0), size: fv("Shells Size"), inherit_parent_velocity: ff("Inherit Parent Velocity", 0.0), cylinder_radius: ff("Cylindrical Radius", 0.0), cylinder_height: ff("Cylindrical Height", 0.0), time_dilation: ff("Shells Time Dilation", 1.0), pump_notify_times, reload_shell_delay });
            }
        }
        if fb("Use Reload Particles") {
            if let Some(sys) = fp("Reload_ParticleSystem") {
                let socket_name = fs("ReloadSocketName").unwrap_or_default();
                let socket = if mesh.is_empty() { None } else { find_socket(&set, &mesh, &socket_name) };
                systems.insert(sys.clone());
                w.reload_smoke = Some(ReloadSmokeData { system: sys, socket_name, socket });
            }
        }
        if let Some(s) = &w.muzzle_system { systems.insert(s.clone()); }
        // beam / shells / reload / destroy effects are listed in the table but not exported; the muzzle variants (`MuzzleFlash(Charged)`, `MuzzleWindParticleSystem`, ...) are
        for (k, p) in &w.other_particle_refs { if k.contains("Muzzle") { systems.insert(p.clone()); } }
        if !mesh.is_empty() { w.socket = find_socket(&set, &mesh, &w.muzzle_socket); }
        table.weapons.insert(wid, w);
    }
    // report
    for w in table.weapons.values() {
        println!("{:<46} {:<36} {:<62} socket {:<10}{} persistent {:<5} {}", w.wid.rsplit('/').next().unwrap_or(""), w.actor_class.rsplit('/').next().unwrap_or(""), w.muzzle_system.as_deref().map_or("-", |s| s.rsplit('/').next().unwrap_or("")), w.muzzle_socket, if w.socket_from_data { "" } else { "*" }, w.persistent_fire,
            match &w.socket { Some(s) => format!("[{} on bone {} at {:?} rot {:?}]", s.owner, s.bone, s.location, s.rotation), None => "[socket NOT FOUND]".into() });
        for (k, p) in &w.other_particle_refs { println!("      {k}: {p}"); }
        for (k, v) in &w.light_properties { println!("      {k} = {v}"); }
    }
    println!("{} weapons, {} particle systems; `*` = MuzzleSocketName not set by any Blueprint (default `Muzzle` inferred)", table.weapons.len(), systems.len());
    if list { return; }
    let packages: Vec<String> = systems.into_iter().collect();
    match fn_particles::import::export_all(&set, &packages, &out) {
        Ok(rep) => {
            println!("exported {} systems, {} meshes, {} materials, {} textures to {}", rep.systems.len(), rep.meshes, rep.materials, rep.textures, out.display());
            for (s, c, n) in &rep.unsupported { println!("UNSUPPORTED in {s}: {c} x{n}"); }
            for f in &rep.failures { println!("FAILED {f}"); }
        }
        Err(e) => { eprintln!("{e}"); std::process::exit(1) }
    }
    std::fs::create_dir_all(&out).ok();
    std::fs::write(out.join("muzzle.json"), serde_json::to_string_pretty(&table).expect("table json")).expect("write muzzle.json");
    println!("wrote {}", out.join("muzzle.json").display());
}
