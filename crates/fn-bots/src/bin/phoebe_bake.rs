use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const ENUMS: &[&str] = &[
    "EExecutionStatus", "EAlertLevel", "EBotMovementState", "EAthenaGamePhaseStep", "EAthenaGamePhase", "ELootElementType", "EPathFollowingResult",
    "EFortSafeZoneState", "EBotControllerClamberStatus", "EOrientedConstructionBuildingType", "EFortWorldMarkerType", "EFortAthenaSmartObjectUrgency",
    "EArithmeticKeyOperation", "EBasicKeyOperation", "EBTFlowAbortMode", "EFocusingBehavior", "EFreeFallingMode", "EBehaviorTreeBranches", "EBTBlackboardRestart",
    "EBTParallelMode", "EBTDecoratorLogic", "ETextKeyOperation", "EBTNodeResult",
];

const CLASS_DEFAULTS: &[(&str, &str)] = &[
    ("BTTask_Wait", r#"{"WaitTime":5.0,"RandomDeviation":0.0,"bIgnoreRestartSelf":false}"#),
    ("BTService", r#"{"Interval":0.5,"RandomDeviation":0.1,"bCallTickOnSearchStart":false,"bRestartTimerOnEachActivation":false}"#),
    ("BTDecorator_Loop", r#"{"NumLoops":3,"bInfiniteLoop":false,"InfiniteLoopTimeoutTime":-1.0}"#),
    ("BTDecorator_Blackboard", r#"{"OperationType":0,"IntValue":0,"FloatValue":0.0,"NotifyObserver":1,"FlowAbortMode":0}"#),
    ("BTDecorator", r#"{"FlowAbortMode":0,"bInverseCondition":false}"#),
    ("BTComposite_SimpleParallel", r#"{"FinishMode":0}"#),
    ("BTComposite", r#"{"bApplyDecoratorScope":false}"#),
    ("BTTask_MoveTo", r#"{"AcceptableRadius":5.0,"bAllowStrafe":false,"bAllowPartialPath":true,"bTrackMovingGoal":true,"bProjectGoalLocation":true,"bReachTestIncludesAgentRadius":true,"bReachTestIncludesGoalRadius":true,"bObserveBlackboardValue":false,"bStopOnOverlapNeedsUpdate":true,"ObservedBlackboardValueTolerance":4.75}"#),
    ("BTTask_MoveDirectlyToward", r#"{"AcceptableRadius":5.0,"bAllowStrafe":false,"bProjectVectorGoalToNavigation":true,"bReachTestIncludesAgentRadius":true,"bReachTestIncludesGoalRadius":true,"bObserveBlackboardValue":false,"bStopOnOverlapNeedsUpdate":true,"ObservedBlackboardValueTolerance":4.75}"#),
    ("FortBTDecorator_DistanceBetween", r#"{"Operator":0,"SpecifiedDistance":0.0,"bCalculateAs2D":false,"DistanceCalculationUpdateRate":0.0}"#),
];

fn class_chain(c: &str) -> Vec<&'static str> {
    let mut v = vec![];
    let bases: &[(&str, &[&str])] = &[
        ("BTTask_Wait", &["BTTask_Wait"]),
        ("BTDecorator_Loop", &["BTDecorator_Loop", "BTDecorator"]),
        ("BTDecorator_Blackboard", &["BTDecorator", "BTDecorator_Blackboard"]),
        ("BTComposite_SimpleParallel", &["BTComposite", "BTComposite_SimpleParallel"]),
        ("BTTask_MoveDirectlyToward", &["BTTask_MoveDirectlyToward"]),
        ("FortAthenaBTTask_BotMoveTo", &["BTTask_MoveTo"]),
        ("FortBTDecorator_DistanceBetween", &["BTDecorator", "FortBTDecorator_DistanceBetween"]),
    ];
    for (k, b) in bases { if *k == c { v.extend(b.iter().copied()); return v; } }
    if c.contains("BTService") { v.push("BTService"); }
    else if c.contains("BTDecorator") { v.push("BTDecorator"); }
    else if c.contains("BTComposite") { v.push("BTComposite"); }
    v
}

fn defaults_for(c: &str) -> (Map<String, Value>, Vec<String>) {
    let mut m = Map::new();
    let mut used = vec![];
    for base in class_chain(c) {
        if let Some((_, d)) = CLASS_DEFAULTS.iter().find(|(k, _)| *k == base) {
            if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(d) { for (k, v) in o { m.insert(k, v); } }
            used.push(base.to_string());
        }
    }
    (m, used)
}

fn short_ref(name: &str) -> String {
    let inner = match (name.find('\''), name.rfind('\'')) { (Some(a), Some(b)) if b > a => &name[a + 1..b], _ => name };
    let inner = inner.rsplit(':').next().unwrap_or(inner);
    inner.rsplit('.').next().unwrap_or(inner).to_string()
}

fn ref_index(v: &Value, pkg: &str) -> Option<i64> {
    let p = v.get("ObjectPath")?.as_str()?;
    let (path, idx) = p.rsplit_once('.')?;
    if path != pkg { return None; }
    idx.parse().ok()
}

fn is_ref(o: &Map<String, Value>) -> bool { o.len() == 2 && o.contains_key("ObjectName") && o.contains_key("ObjectPath") }

fn compact(v: &Value, pkg: &str) -> Value {
    match v {
        Value::Object(o) => {
            if is_ref(o) {
                if let Some(i) = ref_index(v, pkg) { return json!(format!("#{i}")); }
                return json!(short_ref(o["ObjectName"].as_str().unwrap_or("")));
            }
            if o.len() == 1 { if let Some(x) = o.get("SelectedKeyName") { return x.clone(); } if let Some(x) = o.get("TagName") { return x.clone(); } if let Some(x) = o.get("DefaultValue") { return compact(x, pkg); } }
            if o.is_empty() { return Value::Null; }
            if o.contains_key("Curve") && o.get("Curve").map_or(false, |c| c.get("RowName").is_some()) && o.keys().all(|k| k == "Value" || k == "Curve" || k == "RegistryType") {
                let c = &o["Curve"];
                let table = c.get("CurveTable").and_then(|t| t.get("ObjectName")).and_then(|n| n.as_str()).map(short_ref);
                let row = c.get("RowName").and_then(|r| r.as_str()).unwrap_or("None").to_string();
                let value = o.get("Value").cloned();
                return match table {
                    Some(t) => json!({ "v": value.clone().unwrap_or(json!(1.0)), "t": t, "r": row, "vAssumed": value.is_none() }),
                    None => value.unwrap_or(json!(0.0)),
                };
            }
            let mut m = Map::new();
            for (k, x) in o {
                if matches!(k.as_str(), "TreeAsset" | "ParentNode" | "Flags" | "Package" | "Class" | "Outer" | "CachedDescription") { continue; }
                m.insert(k.clone(), compact(x, pkg));
            }
            Value::Object(m)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| compact(x, pkg)).collect()),
        _ => v.clone(),
    }
}

fn idx_of(v: &Value) -> Option<i64> { v.as_str().and_then(|s| s.strip_prefix('#')).and_then(|s| s.parse().ok()) }

fn curve_rows(rows: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for (name, r) in rows {
        let mode = r.get("InterpMode").and_then(|m| m.as_str()).unwrap_or("RCIM_Linear");
        let keys: Vec<Value> = r.get("Keys").and_then(|k| k.as_array()).map(|ks| ks.iter().map(|k| json!([k["Time"], k["Value"]])).collect()).unwrap_or_default();
        out.insert(name.clone(), json!({ "m": interp(mode), "k": keys }));
    }
    Value::Object(out)
}

fn interp(m: &str) -> &'static str { if m.contains("Constant") { "constant" } else if m.contains("Cubic") { "cubic" } else { "linear" } }

fn float_curve(fc: &Value) -> Value {
    let keys = fc.get("Keys").and_then(|k| k.as_array()).cloned().unwrap_or_default();
    let mode = keys.first().and_then(|k| k.get("InterpMode")).and_then(|m| m.as_str()).unwrap_or("RCIM_Linear");
    let pre = fc.get("PreInfinityExtrap").and_then(|m| m.as_str()).unwrap_or("RCCE_Constant");
    let post = fc.get("PostInfinityExtrap").and_then(|m| m.as_str()).unwrap_or("RCCE_Constant");
    json!({ "m": interp(mode), "k": keys.iter().map(|k| json!([k["Time"], k["Value"]])).collect::<Vec<_>>(), "pre": pre.rsplit("::").next().unwrap_or(pre), "post": post.rsplit("::").next().unwrap_or(post) })
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { walk(&p, out); } else if p.extension().map_or(false, |x| x == "json") { out.push(p); }
    }
}

fn read_exe_enums(exe: &Path) -> BTreeMap<String, Vec<(String, i64)>> {
    let mut res = BTreeMap::new();
    let Ok(b) = std::fs::read(exe) else { eprintln!("[bake] exe unreadable: {}", exe.display()); return res };
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
    let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let pe = u32_at(0x3c);
    let ns = u16_at(pe + 6);
    let osz = u16_at(pe + 20);
    let base = u64_at(pe + 24 + 24);
    let mut secs = vec![];
    let mut so = pe + 24 + osz;
    for _ in 0..ns { secs.push((u32_at(so + 12), u32_at(so + 20), u32_at(so + 16))); so += 40; }
    let f2r = |f: usize| secs.iter().find(|s| f >= s.1 && f < s.1 + s.2).map(|s| f - s.1 + s.0);
    let r2f = |r: usize| secs.iter().find(|s| r >= s.0 && r < s.0 + s.2).map(|s| r - s.0 + s.1);
    let cstr = |f: usize| { let mut e = f; while e < b.len() && b[e] != 0 { e += 1; } String::from_utf8_lossy(&b[f..e]).to_string() };
    let find = |needle: &[u8], from: usize| -> Option<usize> { b[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from) };
    for en in ENUMS {
        let needle = format!("{en}::");
        let mut from = 0;
        let mut got = None;
        while let Some(p) = find(needle.as_bytes(), from) {
            from = p + 1;
            if p == 0 || b[p - 1] != 0 { continue; }
            let Some(r) = f2r(p) else { continue };
            let va = (base + r as u64).to_le_bytes();
            let Some(q) = find(&va, 0) else { continue };
            let name_at = |k: usize| -> Option<String> { let pv = u64_at(k); let ff = r2f(pv.checked_sub(base)? as usize)?; let s = cstr(ff); s.starts_with(&needle).then_some(s) };
            let mut s = q;
            while s >= 16 && name_at(s - 16).is_some() { s -= 16; }
            let mut ents = vec![];
            let mut k = s;
            while let Some(n) = name_at(k) { ents.push((n[needle.len()..].to_string(), i64::from_le_bytes(b[k + 8..k + 16].try_into().unwrap()))); k += 16; }
            if !ents.is_empty() { got = Some(ents); break; }
        }
        match got { Some(e) => { res.insert(en.to_string(), e); } None => eprintln!("[bake] enum {en}: not found in the exe") }
    }
    res
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let (Some(src), Some(out)) = (arg("--src"), arg("--out")) else { eprintln!("usage: phoebe-bake --src <uex export dir> --exe <30.0 client exe> --out <assets/bots>"); std::process::exit(2) };
    let exe = arg("--exe");
    let src = PathBuf::from(src);
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).expect("out dir");
    let mut files = vec![];
    walk(&src, &mut files);
    files.sort();
    let mut assets: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        let Ok(Value::Array(j)) = serde_json::from_str::<Value>(&text) else { continue };
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        assets.insert(name, j);
    }
    eprintln!("[bake] {} packages read from {}", assets.len(), src.display());

    let enums = exe.as_deref().map(|e| read_exe_enums(Path::new(e))).unwrap_or_default();

    let mut bb_keys = vec![];
    if let Some(bb) = assets.get("BB_Phoebe") {
        let pkg = bb[0]["Package"].as_str().unwrap_or("").to_string();
        for k in bb[0]["Properties"]["Keys"].as_array().cloned().unwrap_or_default() {
            let name = k["EntryName"].as_str().unwrap_or("").to_string();
            let ti = ref_index(&k["KeyType"], &pkg).unwrap_or(-1);
            let kt = bb.get(ti as usize).cloned().unwrap_or(Value::Null);
            let ty = kt["Type"].as_str().unwrap_or("").trim_start_matches("BlackboardKeyType_").to_string();
            let mut e = Map::new();
            e.insert("n".into(), json!(name));
            e.insert("t".into(), json!(ty));
            if let Some(en) = kt["Properties"].get("EnumName").and_then(|x| x.as_str()) { e.insert("e".into(), json!(en)); }
            else if let Some(en) = kt["Properties"].get("EnumType").and_then(|x| x.get("ObjectName")).and_then(|x| x.as_str()) { e.insert("e".into(), json!(short_ref(en))); }
            if let Some(bc) = kt["Properties"].get("BaseClass").and_then(|x| x.get("ObjectName")).and_then(|x| x.as_str()) { e.insert("c".into(), json!(short_ref(bc))); }
            if k["bInstanceSynced"].as_bool() == Some(true) { e.insert("sync".into(), json!(true)); }
            bb_keys.push(Value::Object(e));
        }
    }
    let bb_types: BTreeMap<String, (String, Option<String>)> = bb_keys.iter().map(|k| (k["n"].as_str().unwrap().to_string(), (k["t"].as_str().unwrap().to_string(), k.get("e").and_then(|e| e.as_str()).map(String::from)))).collect();

    let mut trees = Map::new();
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    let mut assumed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, exps) in &assets {
        if exps.first().and_then(|e| e["Type"].as_str()) != Some("BehaviorTree") { continue; }
        let pkg = exps[0]["Package"].as_str().unwrap_or("").to_string();
        let bt = compact(&exps[0]["Properties"], &pkg);
        let mut nodes = vec![];
        for (i, e) in exps.iter().enumerate() {
            if i == 0 { nodes.push(Value::Null); continue; }
            let class = e["Type"].as_str().unwrap_or("").to_string();
            *classes.entry(class.clone()).or_default() += 1;
            let (mut props, used) = defaults_for(&class);
            for u in used { let a = assumed.entry(class.clone()).or_default(); if !a.contains(&u) { a.push(u); } }
            let given = compact(e.get("Properties").unwrap_or(&Value::Null), &pkg);
            let mut explicit = vec![];
            if let Value::Object(g) = given { for (k, v) in g { explicit.push(k.clone()); props.insert(k, v); } }
            let mut n = Map::new();
            n.insert("c".into(), json!(class));
            n.insert("name".into(), json!(e["Name"]));
            if let Some(Value::Array(ch)) = props.remove("Children") {
                let ch: Vec<Value> = ch.iter().map(|c| {
                    let node = idx_of(&c["ChildComposite"]).or_else(|| idx_of(&c["ChildTask"]));
                    let decos: Vec<i64> = c["Decorators"].as_array().map(|d| d.iter().filter_map(idx_of).collect()).unwrap_or_default();
                    let ops = c.get("DecoratorOps").cloned().unwrap_or(json!([]));
                    json!({ "n": node, "d": decos, "ops": ops })
                }).collect();
                n.insert("ch".into(), Value::Array(ch));
            }
            if let Some(Value::Array(sv)) = props.remove("Services") { n.insert("s".into(), json!(sv.iter().filter_map(idx_of).collect::<Vec<_>>())); }
            if class == "BTDecorator_Blackboard" {
                if let Some(key) = props.get("BlackboardKey").and_then(|k| k.as_str()) {
                    if let Some((t, en)) = bb_types.get(key) {
                        props.insert("KeyType".into(), json!(t));
                        let basic = matches!(t.as_str(), "Bool" | "Object" | "Vector" | "Rotator" | "Class");
                        props.insert("OpKind".into(), json!(if basic { "Basic" } else if t == "String" || t == "Name" { "Text" } else { "Arithmetic" }));
                        if let Some(en) = en { props.insert("Enum".into(), json!(en)); }
                    }
                }
            }
            n.insert("explicit".into(), json!(explicit));
            n.insert("p".into(), Value::Object(props));
            nodes.push(Value::Object(n));
        }
        let root = bt.get("RootNode").and_then(idx_of);
        let rdec: Vec<i64> = bt.get("RootDecorators").and_then(|d| d.as_array()).map(|d| d.iter().filter_map(idx_of).collect()).unwrap_or_default();
        trees.insert(name.clone(), json!({ "bb": bt.get("BlackboardAsset"), "root": root, "rootDecorators": rdec, "nodes": nodes }));
    }

    let mut evaluators = Map::new();
    let mut skillsets = Map::new();
    let mut curves = Map::new();
    let mut generic = Map::new();
    for (name, exps) in &assets {
        let ty = exps.first().and_then(|e| e["Type"].as_str()).unwrap_or("");
        let pkg = exps[0]["Package"].as_str().unwrap_or("").to_string();
        if ty == "BehaviorTree" || name == "BB_Phoebe" { continue; }
        if ty == "CurveTable" { if let Some(r) = exps[0]["Rows"].as_object() { curves.insert(name.clone(), curve_rows(r)); } continue; }
        if ty == "CurveFloat" { let fc = exps[0].get("Properties").and_then(|p| p.get("FloatCurve")).unwrap_or(&exps[0]["FloatCurve"]); curves.insert(name.clone(), json!({ "": float_curve(fc) })); continue; }
        let class = exps.iter().find(|e| e["Type"] == "BlueprintGeneratedClass");
        let cdo = exps.iter().find(|e| e["Name"].as_str().map_or(false, |n| n.starts_with("Default__")));
        if let (Some(c), Some(d)) = (class, cdo) {
            let sup = c.get("SuperStruct").and_then(|s| s.get("ObjectName")).and_then(|s| s.as_str()).map(short_ref).unwrap_or_default();
            let mut subs = Map::new();
            for (i, e) in exps.iter().enumerate() {
                if e["Type"] == "BlueprintGeneratedClass" || e["Name"].as_str().map_or(false, |n| n.starts_with("Default__")) { continue; }
                subs.insert(format!("#{i}"), json!({ "c": e["Type"], "p": compact(e.get("Properties").unwrap_or(&Value::Null), &pkg) }));
            }
            let entry = json!({ "class": c["Name"], "super": sup, "p": compact(d.get("Properties").unwrap_or(&Value::Null), &pkg), "subobjects": subs });
            if name.starts_with("Phoebe_Evaluator") || name.starts_with("Phoebe_EvasiveManeuvers") || name.starts_with("Phoebe_HandleFocusing") || name.starts_with("Phoebe_WeaponADS") { evaluators.insert(name.clone(), entry); }
            else if name.to_ascii_lowercase().starts_with("ai_skill_phoebe_bot_") { skillsets.insert(name.clone(), entry); }
            else { generic.insert(name.clone(), entry); }
            continue;
        }
        let list: Vec<Value> = exps.iter().map(|e| {
            let mut m = Map::new();
            m.insert("type".into(), e["Type"].clone());
            m.insert("name".into(), e["Name"].clone());
            if let Some(p) = e.get("Properties") { m.insert("p".into(), compact(p, &pkg)); }
            if let Some(r) = e.get("Rows") { m.insert("rows".into(), compact(r, &pkg)); }
            Value::Object(m)
        }).collect();
        generic.insert(name.clone(), Value::Array(list));
    }
    let enum_json: Map<String, Value> = enums.iter().map(|(k, v)| (k.clone(), Value::Object(v.iter().map(|(n, x)| (n.clone(), json!(x))).collect()))).collect();
    let src_note = json!({ "game": "Fortnite 30.00-CL-33962396", "tool": "fn-bots phoebe-bake", "uexExport": "Athena/AI/Phoebe, AISettings/PlayerBotsFlow_AISettings, BRCosmetics Athena/AI/Phoebe", "enumsFrom": "FortniteClient-Win64-Shipping.exe static UEnum tables" });
    let class_defaults: Map<String, Value> = CLASS_DEFAULTS.iter().map(|(k, v)| (k.to_string(), serde_json::from_str(v).unwrap())).collect();
    let bt = json!({ "source": src_note, "enums": enum_json, "classDefaults": class_defaults, "classDefaultsApplied": assumed, "classes": classes, "trees": trees });
    let bb = json!({ "source": src_note, "name": "BB_Phoebe", "keys": bb_keys });
    let skills = json!({ "source": src_note, "skillsets": skillsets, "curves": curves });
    let ev = json!({ "source": src_note, "evaluators": evaluators });
    let data = json!({ "source": src_note, "assets": generic });
    let write = |n: &str, v: &Value| { let p = out.join(n); let s = serde_json::to_string(v).unwrap(); std::fs::write(&p, &s).expect("write"); eprintln!("[bake] wrote {} ({} bytes)", p.display(), s.len()); };
    write("phoebe_bt.json", &bt);
    write("phoebe_bb.json", &bb);
    write("phoebe_skills.json", &skills);
    write("phoebe_evaluators.json", &ev);
    write("phoebe_data.json", &data);
    eprintln!("[bake] {} trees, {} bb keys, {} skill sets, {} curve assets, {} evaluator classes, {} data assets, {} enums", trees.len(), bb_keys.len(), skillsets.len(), curves.len(), evaluators.len(), generic.len(), enums.len());
}
