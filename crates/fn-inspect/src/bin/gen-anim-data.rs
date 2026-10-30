//! `gen-anim-data [--root <install>] [--legacy] [out.rs]`: generates `crates/fn-animgraph/src/anim_data.rs` (BlendSpaces, per-clip metadata, blend profiles)
//! straight from the 1.8 paks, in pure Rust. Port of the former Node tool `tools/animation-bs-evidence.cjs` (deleted): the same extraction and the same
//! number / text formatting, so `--legacy` (the original 18 BlendSpaces + 145 clips) reproduces the previously checked-in file byte for byte.
//! Read-only on the install; nothing is decrypted (encrypted entries are not touched). A base name that exists in two folders resolves to the first pak-index entry (paks sorted by name, entries in pak order), as the Node tool did.
//!
//! What it reads: BlendSpace / BlendSpace1D / AimOffsetBlendSpace exports (BlendParameters, InterpolationParam, SampleData, GridSamples, target weight speed,
//! axis-to-scale, marker sample index, mesh-space rotation), AnimSequence metadata (SequenceLength, RateScale, AdditiveAnimType, RefPoseSeq,
//! AuthoredSyncMarkers) and the FastFeet / SlowTorso BlendProfiles of the player skeleton.
use fn_pak::PakSet;
use fn_uasset::PackageData;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

const DEFAULT_ROOT: &str = r"C:\Users\Brady\Downloads\1.8\Fortnite\Fortnite\FortniteGame";

/// (rust key, asset name). Serialized order = ALL_BLENDSPACES order. The first 18 are the original set (the equipped-melee locomotion path + the AR jog /
/// crouch additive spaces + lean / jump spaces).
const BS_BASE: &[(&str, &str)] = &[
    ("walk_core", "M_Locomotion2D_Walk_AR_Core"),
    ("walk_core_rt", "M_Locomotion2D_Walk_AR_Core_RtFootFwd"),
    ("walk_add", "M_Locomotion2D_Walk_AR_Additive"),
    ("crouch_targeting_core", "Crouch_ARTargeting_Core"),
    ("jog_core", "M_Jog_AR_Core"),
    ("jog_core_no45", "M_Jog_AR_Core_No45s"),
    ("crouch_core", "Crouch_ARNonTargeted_Core"),
    ("jog_add", "M_Jog_Melee_TH_Additive"),
    ("crouch_add", "Crouch_TwoHandedMelee_ADD"),
    ("jog_ards_add", "M_Jog_ARDS_Additive"),
    ("jog_arrelaxed_add", "M_Jog_ARRelaxed_Additive"),
    ("crouch_arnt_add", "Crouch_ARNonTargeted_ADD"),
    ("crouch_artg_add", "Crouch_ARTargeting_ADD"),
    ("crouch_arrelaxed_add", "Crouch_ARRelaxed_ADD"),
    ("aim_offset", "M_Melee_TwoHanded_AO"),
    ("jump_lean", "M_Jump_ARDS_Lean"),
    ("lean_add", "JogAdditiveLean"),
    ("crouch_lean_add", "Crouch_Lean"),
];

/// The remaining CoreAnims_Male additive spaces (`JogAdditiveBlendspace*`, `Crouch*AdditiveBlendspace*` of every BR weapon class, see
/// `crates/fn-animgraph/src/anim_core.rs`) that the first set did not carry. Appended after BS_BASE.
const BS_EXTRA: &[(&str, &str)] = &[
    ("jog_ar_add", "M_Jog_AR_Additive"),
    ("jog_pistolhf_add", "M_Jog_PistolHF_Additive"),
    ("jog_melee_oh_add", "M_Jog_Melee_OH_Additive"),
    ("jog_heavy_add", "HeavyWeapon_AdditiveBS"),
    ("jog_nowep_add", "M_Jog_NoWep_Additive"),
    ("jog_decotool_add", "M_Jog_AbilityDecoTool_Additive"),
    ("crouch_pistol_tg_add", "Crouch_PistolTargeted_ADD"),
    ("crouch_pistol_nt_add", "Crouch_PistolNonTargeted_ADD"),
    ("crouch_pistol_rl_add", "Crouch_PistolRelaxed_ADD"),
    ("crouch_nowep_add", "Crouch_NoWep_ADD"),
    ("crouch_launcher_add", "Crouch_Launcher_ADD"),
    ("walk_pistol_add", "M_Locomotion2D_Walk_Pistol_Additive"),
];

const SKY: &str = "/Game/Animation/Game/MainPlayer/Parachute/Medium";
const BS_SKYDIVE: &[(&str, &str)] = &[
    ("sky_glide_2d", "Female/BS_Skydive_Glide_2D"),
    ("sky_glide_lean", "Female/BS_Skydive_Glide_Lean"),
    ("sky_dive_2d", "Female/BS_Skydive_Dive_2D"),
    ("sky_dive_lean", "Female/BS_Skydive_Dive_Lean"),
    ("sky_hangglider_into", "Female/BS_Skydive_HangGlider_Into"),
    ("sky_hangglider_m", "Male/BS_Skydive_HangGlider"),
    ("sky_hangglider_f", "Female/BS_Skydive_HangGlider"),
    ("sky_hangglider_lean_m", "Male/BS_Skydive_HangGlider_Lean"),
    ("sky_hangglider_lean_f", "Female/BS_Skydive_HangGlider_Lean"),
];

const DBNO_BS: &str = "/Game/Animation/Game/MainPlayer/Locomotion/Medium/Male/DBNO/BS_DBNO_CMM";
const DBNO_CLIPS: &[&str] = &["/Game/Animation/Game/MainPlayer/Reacts/HitReacts/AthenaDBNO_Loop_CMM", "/Game/Animation/Game/MainPlayer/Locomotion/Medium/Male/DBNO/DBNOCrawl_E_CMM"];
const FEMALE_CLIPS: &[&str] = &["/Game/Animation/Game/MainPlayer/Locomotion/Medium/Female/Turns/Idle_Turn_Left", "/Game/Animation/Game/MainPlayer/Locomotion/Medium/Female/Turns/Idle_Turn_Right", "Pose_Blueprint_Place_CMF", "Pose_Blueprint_Edit_CMF"];

const BS_FEMALE: &[(&str, &str)] = &[
    ("f_jog_core", "AssaultRifle_NonTargeted_CMF_COR"),
    ("f_jog_core_no45", "AssaultRifle_NonTargeted_CMF_COR_No45s"),
    ("f_jog_ar_add", "AssaultRifle_NonTargeted_CMF_Add"),
    ("f_jog_arrelaxed_add", "AssaultRifle_Relaxed_CMF_AD"),
    ("f_jog_arhf_add", "AssaultRifle_HFNT_CMF_Add"),
    ("f_jog_pistol_add", "Pistol_NonTargeted_CMF_ADD"),
    ("f_jog_melee_oh_add", "Melee_OneHanded_CMF_ADD"),
    ("f_jog_melee_th_add", "Melee_TwoHanded_CMF_ADD"),
    ("f_jog_nowep_add", "NoWep_CMF_ADD"),
];

/// Clips played directly by the graph (SequencePlayers / evaluators of the baked state machines) plus the montage / sprint clips.
fn direct_clips() -> Vec<String> {
    let mut d: Vec<String> = ["M_Melee_TwoHanded", "M_NoWep_Relaxed", "Idle_Noise", "Sprint_TwoHanded", "CrouchSprint_THMelee", "Crouch_TwoHanded_Melee_CMM", "Crouch_THMelee_E",
        "JumpLand_NoWep_CMM_Additive", "JumpLandMoving_NoWep_CMM_Additive", "JumpUp_NoWep_CMM_Additive", "JumpFall_NoWep_CMM_Additive", "JumpApex_NoWep__Loop_CMM_Additive",
        "M_JumpUp_ARDS_Additive", "M_JumpApex_ARDS_Loop_Additive", "M_JumpFall_ARDS_Additive", "M_JumpLand_ARDS_Additive", "JumpLand_NoWep_CMM_Additive_Core",
        "M_Jog_Melee_TwoHanded_E_ScaledForTurnInPlace", "M_Jog_Melee_Twohanded_E_Core", "Crouch_ARNonTargeted_E_Core", "Melee_WideGrip_Equip_CMM", "Idle_Turn_Left", "Idle_Turn_Right"]
        .iter().map(|s| s.to_string()).collect();
    for dir in ["N", "E", "S", "W"] { for k in ["Start", "Stop", "Pivot"] { for t in ["Core", "Additive"] { d.push(format!("M_Jog_{dir}_{k}_{t}")); } } }
    for dir in ["N", "NE", "E", "SE", "S", "SW", "W", "NW"] { d.push(format!("M_Jog_Melee_Twohanded_{dir}_Core")); d.push(format!("M_Jog_Melee_Twohanded_{dir}")); }
    d
}

/// JS `String(+v.toPrecision(9))`, plus `.0` when it has neither `.` nor `e` (the .cjs `f`). Float values read from the f32 UE data round-trip through f64.
fn f(v: f64) -> String {
    if v == 0.0 || !v.is_finite() { return if v.is_finite() { "0.0".into() } else { format!("{v}") }; }
    let s = format!("{:.8e}", v.abs());
    let (m, e) = s.split_once('e').unwrap();
    let e: i32 = e.parse().unwrap();
    let digits: String = m.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let (k, n) = (digits.len() as i32, e + 1);
    let mut out = if k <= n && n <= 21 { format!("{digits}{}", "0".repeat((n - k) as usize)) }
        else if 0 < n && n <= 21 { format!("{}.{}", &digits[..n as usize], &digits[n as usize..]) }
        else if -6 < n && n <= 0 { format!("0.{}{digits}", "0".repeat((-n) as usize)) }
        else { let ex = n - 1; let sign = if ex < 0 { '-' } else { '+' }; if k == 1 { format!("{digits}e{sign}{}", ex.abs()) } else { format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], ex.abs()) } };
    if v < 0.0 { out.insert(0, '-'); }
    if out.contains('.') || out.contains('e') { out } else { format!("{out}.0") }
}
fn q(s: &str) -> String { serde_json::to_string(s).unwrap() }
fn num(v: &Value) -> Option<f64> { v.as_f64() }
/// `Foo::Bar` -> `Bar` (enum values), `/Game/x/Y.Y` -> `Y` (object paths).
fn short(s: &str) -> String { let s = s.rsplit("::").next().unwrap(); s.rsplit('/').next().unwrap().split('.').next().unwrap().to_string() }

struct Bs { key: String, name: String, class: String, axes: [(f64, f64, i64, f64, String); 2], tw_speed: f64, axis_to_scale: String, sample_marker: i64, mesh_rot: bool, samples: Vec<(String, [f64; 2], f64)>, grid: Vec<([i64; 3], [f64; 3])> }
struct Clip { name: String, length: f64, rate: f64, additive: String, ref_pose: String, markers: Vec<(String, f64)> }

struct Paks { set: PakSet, by_base: HashMap<String, Vec<String>> }
impl Paks {
    fn open(root: &str) -> Paks {
        let set = PakSet::open_install(root).unwrap_or_else(|e| panic!("open {root}: {e}"));
        let mut by_base: HashMap<String, Vec<String>> = HashMap::new();
        // pak-index order (paks sorted by name, entries in pak order): the first entry of a base name wins, as the Node tool did (a few clip names exist in two folders)
        let mut paks: Vec<&fn_pak::PakFile> = set.paks().iter().collect();
        paks.sort_by(|a, b| a.name.cmp(&b.name));
        for e in paks.into_iter().flat_map(|pf| pf.entries.iter()).filter(|e| e.path.ends_with(".uasset")) {
            let p = e.path.to_string();
            by_base.entry(p.rsplit('/').next().unwrap().trim_end_matches(".uasset").to_string()).or_default().push(p);
        }
        Paks { set, by_base }
    }
    /// The first export named `name` of the package called `name` (None when the asset is not in the paks).
    fn export_path(&self, long: &str) -> Option<Value> {
        let path = format!("FortniteGame/Content/{}.uasset", long.strip_prefix("/Game/")?);
        let d = PackageData::load(&self.set, &path).ok()?;
        let pkg = d.package().ok()?;
        let i = pkg.find_export(long.rsplit('/').next()?)?;
        Some(pkg.read_export(i).ok()?.to_json(&pkg))
    }
    fn export(&self, name: &str, export: &str) -> Option<Value> {
        let path = self.by_base.get(name)?.first()?;
        let d = PackageData::load(&self.set, path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let pkg = d.package().unwrap_or_else(|e| panic!("{path}: {e}"));
        let i = pkg.find_export(export)?;
        Some(pkg.read_export(i).unwrap_or_else(|e| panic!("{path}: {e}")).to_json(&pkg))
    }
}

fn read_bs(p: &Paks, key: &str, name: &str) -> Bs {
    let by_path = name.starts_with("/Game/");
    let x = if by_path { p.export_path(name) } else { p.export(name, name) }.unwrap_or_else(|| panic!("missing BlendSpace {name}"));
    let pr = &x["properties"];
    let class = x["class"].as_str().unwrap().to_string();
    let ax = |i: usize| {
        let (bk, ik) = if i == 0 { ("BlendParameters".to_string(), "InterpolationParam".to_string()) } else { ("BlendParameters[1]".into(), "InterpolationParam[1]".into()) };
        let (bp, ip) = (&pr[&bk], &pr[&ik]);
        (num(&bp["Min"]).unwrap_or(0.), num(&bp["Max"]).unwrap_or(100.), bp["GridNum"].as_i64().unwrap_or(4), num(&ip["InterpolationTime"]).unwrap_or(0.), ip["InterpolationType"].as_str().map(short).unwrap_or_else(|| "BSIT_Average".into()))
    };
    let axes = [ax(0), ax(1)];
    let samples: Vec<_> = pr["SampleData"].as_array().unwrap().iter().map(|s| {
        let sv = s["SampleValue"].as_array().unwrap();
        (if by_path { s["Animation"].as_str().unwrap().split('.').next().unwrap().to_string() } else { short(s["Animation"].as_str().unwrap()) }, [num(&sv[0]).unwrap_or(0.), num(&sv[1]).unwrap_or(0.)], num(&s["RateScale"]).unwrap_or(1.))
    }).collect();
    let fld = |g: &Value, k: &str, i: usize| g[if i == 0 { k.to_string() } else { format!("{k}[{i}]") }].clone();
    let grid: Vec<_> = pr["GridSamples"].as_array().unwrap().iter().map(|g| {
        let mut idx = [-1i64; 3]; let mut w = [0f64; 3];
        for i in 0..3 { idx[i] = fld(g, "Indices", i).as_i64().unwrap_or(-1); w[i] = fld(g, "Weights", i).as_f64().unwrap_or(0.); }
        (idx, w)
    }).collect();
    let one_d = class == "BlendSpace1D";
    let cells = if one_d { axes[0].2 + 1 } else { (axes[0].2 + 1) * (axes[1].2 + 1) };
    assert_eq!(grid.len() as i64, cells, "{name}: grid element count");
    Bs { key: key.into(), name: name.into(), class, axes, tw_speed: num(&pr["TargetWeightInterpolationSpeedPerSec"]).unwrap_or(0.), axis_to_scale: pr["AxisToScaleAnimation"].as_str().map(short).unwrap_or_else(|| "BSA_None".into()),
        sample_marker: pr["SampleIndexWithMarkers"].as_i64().unwrap_or(-1), mesh_rot: pr["bRotationBlendInMeshSpace"].as_bool().unwrap_or(false), samples, grid }
}

fn read_clip(p: &Paks, name: &str) -> Option<Clip> {
    let full = name.strip_prefix("/Game/").map(|r| format!("FortniteGame/Content/{r}.uasset"));
    let path = match &full { Some(f) => f, None => p.by_base.get(name)?.first()? };
    let d = PackageData::load(&p.set, path).unwrap();
    let pkg = d.package().unwrap();
    let i = pkg.find_export(name.rsplit('/').next().unwrap()).or_else(|| (0..pkg.exports.len()).find(|&i| pkg.export_class_name(i) == "AnimSequence"))?;
    let x = pkg.read_export(i).unwrap().to_json(&pkg);
    let pr = &x["properties"];
    let markers = pr["AuthoredSyncMarkers"].as_array().map(|a| a.iter().map(|m| (m["MarkerName"].as_str().unwrap().to_string(), m["Time"].as_f64().unwrap_or(0.))).collect()).unwrap_or_default();
    Some(Clip { name: name.into(), length: num(&pr["SequenceLength"]).unwrap_or(0.), rate: num(&pr["RateScale"]).unwrap_or(1.), additive: pr["AdditiveAnimType"].as_str().map(short).unwrap_or_else(|| "AAT_None".into()),
        ref_pose: pr["RefPoseSeq"].as_str().map(short).unwrap_or_default(), markers })
}

fn main() {
    let mut root = std::env::var("FN_ROOT").unwrap_or_else(|_| DEFAULT_ROOT.to_string());
    let (mut legacy, mut out_path) = (false, concat!(env!("CARGO_MANIFEST_DIR"), "/../fn-animgraph/src/anim_data.rs").to_string());
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() { match a.as_str() { "--root" => root = it.next().unwrap(), "--legacy" => legacy = true, o => out_path = o.to_string() } }
    let p = Paks::open(&root);
    let list: Vec<(&str, &str)> = BS_BASE.iter().chain(if legacy { [].iter() } else { BS_EXTRA.iter() }).chain(if legacy { [].iter() } else { BS_FEMALE.iter() }).copied().collect();
    let mut bs: Vec<Bs> = list.iter().map(|(k, n)| read_bs(&p, k, n)).collect();
    if !legacy { for (k, n) in BS_SKYDIVE { bs.push(read_bs(&p, k, &format!("{SKY}/{n}"))); } }
    if !legacy { bs.push(read_bs(&p, "dbno_crawl", DBNO_BS)); }
    let mut names: BTreeSet<String> = bs.iter().flat_map(|b| b.samples.iter().map(|s| s.0.clone())).collect();
    names.extend(direct_clips());
    if !legacy { names.extend(FEMALE_CLIPS.iter().map(|s| s.to_string())); }
    if !legacy { names.extend(DBNO_CLIPS.iter().map(|s| s.to_string())); }
    let mut missing = vec![];
    let clips: Vec<Clip> = names.iter().filter_map(|n| { let c = read_clip(&p, n); if c.is_none() { missing.push(n.clone()); } c }).collect();
    let skel = |profile: &str| -> Vec<(String, f64)> {
        let x = p.export("Fortnite_M_Avg_Player_Skeleton", profile).unwrap_or_else(|| panic!("skeleton blend profile {profile} missing"));
        x["properties"]["ProfileEntries"].as_array().unwrap().iter().map(|i| (i["BoneReference"]["BoneName"].as_str().unwrap().to_lowercase(), i["BlendScale"].as_f64().unwrap())).collect()
    };
    // `+i.BlendScale.toFixed(6)` in the .cjs: round to 6 decimals first, then the shared formatter
    let r6 = |v: f64| (v * 1e6).round() / 1e6;

    let mut rs = String::from("// GENERATED by `cargo run -p fn-inspect --bin gen-anim-data` (crates/fn-inspect/src/bin/gen-anim-data.rs) from the original paks -- do not edit by hand.\n// Types (`BsDef`, `Axis`, ...) are defined in anim_graph.rs, which include!s this file.\n");
    for b in &bs {
        let id = b.key.to_uppercase();
        rs += &format!("pub static {id}_SAMPLES:&[BsSample]=&[{}];\n", b.samples.iter().map(|s| format!("BsSample{{anim:{},pos:[{},{}],rate:{}}}", q(&s.0), f(s.1[0]), f(s.1[1]), f(s.2))).collect::<Vec<_>>().join(","));
        rs += &format!("pub static {id}_GRID:&[GridElem]=&[{}];\n", b.grid.iter().map(|g| format!("GridElem{{idx:[{}],w:[{}]}}", g.0.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(","), g.1.iter().map(|w| f(*w)).collect::<Vec<_>>().join(","))).collect::<Vec<_>>().join(","));
        let ax = |a: &(f64, f64, i64, f64, String)| format!("Axis{{min:{},max:{},grid:{},interp_time:{},interp_type:{}}}", f(a.0), f(a.1), a.2, f(a.3), q(&a.4));
        rs += &format!("pub static {id}:BsDef=BsDef{{name:{},class:{},axes:[{},{}],target_weight_speed:{},axis_to_scale:{},sample_index_with_markers:{},mesh_space_rotation:{},samples:{id}_SAMPLES,grid:{id}_GRID}};\n", q(&b.name), q(&b.class), ax(&b.axes[0]), ax(&b.axes[1]), f(b.tw_speed), q(&b.axis_to_scale), b.sample_marker, b.mesh_rot);
    }
    rs += &format!("pub static ALL_BLENDSPACES:&[&BsDef]=&[{}];\n", bs.iter().map(|b| format!("&{}", b.key.to_uppercase())).collect::<Vec<_>>().join(","));
    rs += &format!("pub static CLIP_META:&[ClipMeta]=&[{}];\n", clips.iter().map(|c| format!("ClipMeta{{name:{},length:{},rate_scale:{},additive:{},ref_pose:{},markers:&[{}]}}", q(&c.name), f(c.length), f(c.rate), q(&c.additive), q(&c.ref_pose), c.markers.iter().map(|m| format!("({},{})", q(&m.0), f(m.1))).collect::<Vec<_>>().join(","))).collect::<Vec<_>>().join(","));
    for (n, id) in [("FastFeet", "FAST_FEET"), ("SlowTorso", "SLOW_TORSO")] {
        rs += &format!("pub static {id}:&[(&str,f32)]=&[{}];\n", skel(n).iter().map(|i| format!("({},{})", q(&i.0), f(r6(i.1)))).collect::<Vec<_>>().join(","));
    }
    std::fs::write(&out_path, &rs).unwrap_or_else(|e| panic!("write {out_path}: {e}"));
    println!("wrote {out_path}: {} blendspaces, {} clips, {} bytes; missing from paks: {:?}", bs.len(), clips.len(), rs.len(), missing);
}
