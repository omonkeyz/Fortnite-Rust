use serde_json::Value;
use std::collections::HashMap;

pub struct Phoebe {
    pub bt: Value,
    pub bb: Value,
    pub skills: Value,
    pub evaluators: Value,
    pub data: Value,
    pub enums: HashMap<String, HashMap<String, i32>>,
    pub curves: HashMap<String, HashMap<String, crate::curve::Curve>>,
}

pub const FILES: [&str; 5] = ["phoebe_bt.json", "phoebe_bb.json", "phoebe_skills.json", "phoebe_evaluators.json", "phoebe_data.json"];

impl Phoebe {
    pub fn parse(bt: &str, bb: &str, skills: &str, evaluators: &str, data: &str) -> Result<Phoebe, String> {
        let p = |s: &str, n: &str| serde_json::from_str::<Value>(s).map_err(|e| format!("{n}: {e}"));
        let bt = p(bt, "phoebe_bt.json")?;
        let skills = p(skills, "phoebe_skills.json")?;
        let mut enums = HashMap::new();
        if let Some(o) = bt["enums"].as_object() {
            for (k, v) in o { enums.insert(k.clone(), v.as_object().map(|m| m.iter().map(|(n, x)| (n.clone(), x.as_i64().unwrap_or(0) as i32)).collect()).unwrap_or_default()); }
        }
        let mut curves = HashMap::new();
        if let Some(o) = skills["curves"].as_object() {
            for (table, rows) in o {
                let mut t = HashMap::new();
                if let Some(r) = rows.as_object() { for (row, c) in r { t.insert(row.clone(), crate::curve::Curve::from_json(c)); } }
                curves.insert(table.clone(), t);
            }
        }
        Ok(Phoebe { bt, bb: p(bb, "phoebe_bb.json")?, skills, evaluators: p(evaluators, "phoebe_evaluators.json")?, data: p(data, "phoebe_data.json")?, enums, curves })
    }
    pub fn load_dir(dir: &std::path::Path) -> Result<Phoebe, String> {
        let r = |n: &str| std::fs::read_to_string(dir.join(n)).map_err(|e| format!("{}: {e}", dir.join(n).display()));
        Phoebe::parse(&r(FILES[0])?, &r(FILES[1])?, &r(FILES[2])?, &r(FILES[3])?, &r(FILES[4])?)
    }
    pub fn enum_value(&self, en: &str, name: &str) -> i32 { self.enums.get(en).and_then(|e| e.get(name)).copied().unwrap_or(-1) }
    pub fn curve(&self, table: &str, row: &str) -> Option<&crate::curve::Curve> { self.curves.get(table)?.get(row) }
    pub fn row(&self, table: &str, row: &str, at: f32) -> Option<f32> { self.curve(table, row).map(|c| c.eval(at)) }
    pub fn evaluator_class<'a>(&'a self, class: &'a str) -> (String, Option<&'a Value>) {
        let key = class.trim_start_matches("BlueprintGenerated").trim_end_matches("_C");
        match self.evaluators["evaluators"].get(key) {
            Some(e) => {
                let sup = e["super"].as_str().unwrap_or("");
                let native = if !sup.is_empty() && !sup.ends_with("_C") { sup.to_string() } else if key.starts_with("Phoebe_EvasiveManeuvers") { "FortAthenaAIBotEvaluator_EvasiveManeuvers".to_string() } else { sup.to_string() };
                (native, Some(&e["p"]))
            }
            None => (key.to_string(), None),
        }
    }
}
