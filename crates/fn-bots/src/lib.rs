pub mod bb;
pub mod brain;
pub mod bt;
pub mod curve;
pub mod data;
pub mod skill;

pub use brain::{Agent, Body, Bot, Bus, FireRequest, Intent, Loot, Poi, Sound, Storm, Weapon, World};
pub use data::Phoebe;

use std::sync::Arc;

pub struct Phoebes { pub data: Arc<Phoebe>, pub graph: Arc<bt::Graph> }

impl Phoebes {
    pub fn new(data: Phoebe) -> Result<Phoebes, String> {
        let schema = Arc::new(bb::Schema::from_json(&data.bb));
        let graph = bt::Graph::build(&data.bt, schema, "BT_Phoebe")?;
        Ok(Phoebes { data: Arc::new(data), graph: Arc::new(graph) })
    }
    pub fn spawn(&self, id: u64, team: u32, name: String, seed: u64) -> Bot {
        let (lo, hi) = skill::level_range(&self.data);
        let u = ((seed.wrapping_mul(0x9e3779b97f4a7c15) >> 40) as f32) / (1u64 << 24) as f32;
        Bot::new(self.data.clone(), &self.graph, id, team, name, lo + (hi - lo) * u, seed)
    }
    pub fn node_report(&self) -> Vec<(String, brain::Origin, &'static str, usize)> {
        let mut m: std::collections::BTreeMap<String, usize> = Default::default();
        for n in &self.graph.nodes { *m.entry(n.class.clone()).or_default() += 1; }
        for a in &self.graph.aux {
            *m.entry(a.class.clone()).or_default() += 1;
            if a.class == "FortAthenaBTService_AIEvaluator" {
                if let Some(c) = a.s("AIEvaluatorClass") { let (native, _) = self.data.evaluator_class(c); *m.entry(native).or_default() += 1; }
            }
        }
        m.into_iter().map(|(c, n)| { let (o, why) = brain::node_origin(&c); (c, o, why, n) }).collect()
    }
}

#[cfg(test)]
mod tests;
