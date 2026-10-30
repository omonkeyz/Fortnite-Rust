use fn_anim::fnanim::FnBone;
use fn_anim::{CurveKey, FloatCurve, FnAnim, Precision, SkeletonInfo, TranslationRetarget};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn f(v: &Value) -> f32 { v.as_f64().unwrap_or(0.) as f32 }

fn chunks<const N: usize>(v: &Value) -> Vec<[f32; N]> {
    let a: Vec<f32> = v.as_array().map(|a| a.iter().map(f).collect()).unwrap_or_default();
    a.chunks_exact(N).map(|c| { let mut o = [0.; N]; o.copy_from_slice(c); o }).collect()
}

fn collapse<const N: usize>(k: Vec<[f32; N]>) -> Vec<[f32; N]> {
    if k.len() > 1 && k.iter().all(|x| x.iter().zip(&k[0]).all(|(a, b)| (a - b).abs() <= 1e-7)) { vec![k[0]] } else { k }
}

struct Skel30 { names: Vec<String>, modes: Vec<TranslationRetarget>, ref_t: Vec<[f32; 3]> }

fn load_skel30(p: &Path) -> Skel30 {
    let j: Value = serde_json::from_slice(&std::fs::read(p).expect("30.0 skeleton json")).expect("json");
    let bones = j["bones"].as_array().expect("bones");
    Skel30 {
        names: bones.iter().map(|b| b["name"].as_str().unwrap_or("").to_owned()).collect(),
        modes: bones.iter().map(|b| TranslationRetarget::from_u8(b["retarget"].as_i64().unwrap_or(0).max(0) as u8)).collect(),
        ref_t: bones.iter().map(|b| { let t = &b["t"]; [f(&t[0]), f(&t[1]), f(&t[2])] }).collect(),
    }
}

fn bake(j: &Value, s30: &Skel30, skel: &SkeletonInfo) -> (FnAnim, Vec<String>) {
    let mut notes = vec![];
    let props = &j["props"];
    let pp = if props.get("Properties").is_some() { &props["Properties"] } else { props };
    let root_motion = pp["bEnableRootMotion"].as_bool().unwrap_or(false);
    let force_lock = pp["bForceRootLock"].as_bool().unwrap_or(false);
    let src_pose: Option<Vec<[f32; 3]>> = j["retarget_pose"].as_array().map(|a| a.iter().map(|b| { let t = &b["t"]; [f(&t[0]), f(&t[1]), f(&t[2])] }).collect());
    let tracks = j["bones"].as_array().cloned().unwrap_or_default();
    let nk = j["num_keys"].as_u64().unwrap_or(1).max(1) as usize;
    let mut used = 0;
    let bones = skel.bone_names.iter().enumerate().map(|(i, name)| {
        let i30 = s30.names.iter().position(|n| n.eq_ignore_ascii_case(name));
        let mode = i30.map_or(skel.retarget[i], |k| s30.modes[k]);
        let source = i30.map_or(skel.ref_pose[i].translation, |k| src_pose.as_ref().and_then(|p| p.get(k).copied()).unwrap_or(s30.ref_t[k]));
        let mut fb = FnBone { name: name.clone(), retarget: mode, retarget_source: source, pos: None, rot: None, scale: None };
        if let Some(t) = tracks.iter().find(|t| t["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name))) {
            used += 1;
            if !(i == 0 && (root_motion || force_lock)) {
                let pos = chunks::<3>(&t["pos"]); let rot = chunks::<4>(&t["rot"]); let scl = chunks::<3>(&t["scl"]);
                if pos.len() == nk { fb.pos = Some(collapse(pos)); }
                if rot.len() == nk { fb.rot = Some(collapse(rot)); }
                if scl.len() == nk { fb.scale = Some(collapse(scl)); }
            }
        }
        fb
    }).collect();
    let dropped: Vec<String> = tracks.iter().filter_map(|t| t["name"].as_str()).filter(|n| !skel.bone_names.iter().any(|b| b.eq_ignore_ascii_case(n))).map(str::to_owned).collect();
    if !dropped.is_empty() { notes.push(format!("{} 30.0-only tracks dropped: {}", dropped.len(), dropped.join(" "))); }
    notes.push(format!("{used}/{} tracks mapped onto the 1.8 skeleton", tracks.len()));
    let curves = j["curves"].as_array().map(|a| a.iter().map(|c| FloatCurve {
        name: c["name"].as_str().unwrap_or("").to_owned(),
        flags: c["flags"].as_i64().unwrap_or(0) as i32,
        default_value: 0.,
        keys: c["keys"].as_array().map(|ks| ks.iter().map(|k| CurveKey { time: f(&k[0]), value: f(&k[1]), arrive_tangent: f(&k[2]), leave_tangent: f(&k[3]), interp_mode: k[4].as_u64().unwrap_or(0) as u8 }).collect()).unwrap_or_default(),
    }).collect()).unwrap_or_default();
    let length = f(&j["sequence_length"]);
    let rate = f(&j["rate_scale"]);
    let additive = j["additive"].as_i64().unwrap_or(0) as u8;
    let fa = FnAnim {
        name: j["name"].as_str().unwrap_or("").to_owned(),
        skeleton: "Fortnite_M_Avg_Player_Skeleton".into(),
        retarget_source: "None".into(),
        ref_pose_seq: j["ref_pose_seq"].as_str().unwrap_or("").to_owned(),
        num_frames: nk,
        length,
        rate_scale: rate,
        frame_rate: if length > 0. { nk as f32 / length * rate } else { 0. },
        additive: additive != 0,
        additive_kind: additive,
        ref_pose_type: j["ref_pose_type"].as_i64().unwrap_or(0) as u8,
        ref_frame_index: j["ref_frame_index"].as_i64().unwrap_or(0) as i32,
        retarget_base: false,
        root_motion,
        step: j["interp"].as_i64() == Some(1),
        bones,
        notifies: vec![],
        sync_markers: vec![],
        curves,
    };
    (fa, notes)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 { eprintln!("usage: fn-enhanced-bake <raw dumper dir> <1.8 install or Paks dir> <out dir>"); std::process::exit(2); }
    let raw = PathBuf::from(&a[1]);
    let paks = fn_pak::PakSet::open_install(&a[2]).or_else(|_| fn_pak::PakSet::open_dir(&a[2])).expect("1.8 paks");
    let src = fn_pose::ClipSourceDir::Pak(&paks);
    let skel = SkeletonInfo::from_package(&fn_pose::loader::package(&src, fn_pose::loadout::PLAYER_SKELETON).expect("1.8 skeleton")).expect("skeleton info");
    let s30 = load_skel30(&raw.join("Fortnite_M_Avg_Player_Skeleton.json"));
    let out = PathBuf::from(&a[3]);
    std::fs::create_dir_all(&out).expect("out dir");
    let mut report = vec![];
    for (i, n) in skel.bone_names.iter().enumerate() {
        let Some(k) = s30.names.iter().position(|x| x.eq_ignore_ascii_case(n)) else { report.push(format!("bone {n}: missing in 30.0")); continue };
        if s30.modes[k] != skel.retarget[i] { report.push(format!("bone {n}: retarget mode 1.8 {:?} vs 30.0 {:?}", skel.retarget[i], s30.modes[k])); }
        let d: f32 = (0..3).map(|c| (s30.ref_t[k][c] - skel.ref_pose[i].translation[c]).powi(2)).sum::<f32>().sqrt();
        if d > 0.05 { report.push(format!("bone {n}: ref translation differs by {d:.3} cm")); }
    }
    let mut total = 0usize;
    let mut index = vec![];
    let mut files: Vec<_> = std::fs::read_dir(&raw).expect("raw dir").flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    for p in files {
        let j: Value = match serde_json::from_slice(&std::fs::read(&p).unwrap_or_default()) { Ok(j) => j, Err(_) => continue };
        if j["kind"].as_str() != Some("sequence") { continue; }
        let (fa, notes) = bake(&j, &s30, &skel);
        let mut bytes = vec![];
        fa.write(&mut bytes, Precision::Q16);
        let back = FnAnim::read(&bytes).expect("read back");
        assert_eq!(back.num_frames, fa.num_frames);
        let file = out.join(format!("{}.fnanim", fa.name));
        std::fs::write(&file, &bytes).expect("write");
        total += bytes.len();
        report.push(format!("{}: {} keys, {:.4} s, rate {}, {} bytes; {}", fa.name, fa.num_frames, fa.length, fa.rate_scale, bytes.len(), notes.join("; ")));
        index.push(serde_json::json!({"name": fa.name, "file": format!("{}.fnanim", fa.name), "length": fa.length, "keys": fa.num_frames, "rate_scale": fa.rate_scale, "source": j["path"]}));
    }
    std::fs::write(out.join("index.json"), serde_json::to_vec_pretty(&serde_json::json!({"clips": index})).unwrap()).expect("index");
    report.push(format!("total {total} bytes"));
    std::fs::write(raw.join("bake_report.txt"), report.join("\n")).expect("report");
    println!("{}", report.join("\n"));
}
