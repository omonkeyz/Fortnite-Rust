use crate::bb::{test_arith, test_basic, Blackboard, OpKind, Schema};
use serde_json::{Map, Value};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind { Selector, Sequence, Parallel, Subtree, Task }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status { Succeeded, Failed, InProgress }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AuxKind { Decorator, Service }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Abort { None, LowerPriority, Selff, Both }

impl Abort {
    fn from(i: i64) -> Abort { match i { 1 => Abort::LowerPriority, 2 => Abort::Selff, 3 => Abort::Both, _ => Abort::None } }
    pub fn observes_self(self) -> bool { matches!(self, Abort::Selff | Abort::Both) }
    pub fn observes_lower(self) -> bool { matches!(self, Abort::LowerPriority | Abort::Both) }
}

#[derive(Clone, Debug)]
pub enum Deco {
    Blackboard { key: Option<u16>, kind: OpKind, op: i32, int: i32, float: f32, inverse: bool },
    Loop { num: i32, infinite: bool, timeout: f32 },
    ForceSuccess,
    Custom,
}

#[derive(Clone, Debug)]
pub struct Aux {
    pub tree: String,
    pub name: String,
    pub class: String,
    pub kind: AuxKind,
    pub p: Map<String, Value>,
    pub keys: HashMap<String, u16>,
    pub abort: Abort,
    pub deco: Deco,
    pub interval: f32,
    pub deviation: f32,
    pub tick_on_search: bool,
}

#[derive(Clone, Debug)]
pub struct Child { pub node: u32, pub decos: Vec<u32>, pub ops: Vec<(u8, u16)> }

#[derive(Clone, Debug)]
pub struct Node {
    pub tree: String,
    pub name: String,
    pub class: String,
    pub kind: Kind,
    pub p: Map<String, Value>,
    pub keys: HashMap<String, u16>,
    pub children: Vec<Child>,
    pub services: Vec<u32>,
    pub parent: Option<u32>,
}

impl Node {
    pub fn key(&self, prop: &str) -> Option<u16> { self.keys.get(prop).copied() }
    pub fn f(&self, prop: &str) -> f32 { self.p.get(prop).and_then(|v| v.as_f64()).unwrap_or(0.) as f32 }
    pub fn b(&self, prop: &str) -> bool { self.p.get(prop).and_then(|v| v.as_bool()).unwrap_or(false) }
    pub fn s(&self, prop: &str) -> Option<&str> { self.p.get(prop).and_then(|v| v.as_str()) }
    pub fn label(&self) -> String { format!("{}:{}", self.tree, self.p.get("NodeName").and_then(|v| v.as_str()).unwrap_or(&self.name)) }
}

impl Aux {
    pub fn key(&self, prop: &str) -> Option<u16> { self.keys.get(prop).copied() }
    pub fn f(&self, prop: &str) -> f32 { self.p.get(prop).and_then(|v| v.as_f64()).unwrap_or(0.) as f32 }
    pub fn s(&self, prop: &str) -> Option<&str> { self.p.get(prop).and_then(|v| v.as_str()) }
    pub fn label(&self) -> String { format!("{}:{}", self.tree, self.p.get("NodeName").and_then(|v| v.as_str()).unwrap_or(&self.name)) }
}

pub fn combine(ops: &[(u8, u16)], res: &[bool]) -> bool {
    if ops.is_empty() { return res.iter().all(|r| *r); }
    fn eval(ops: &[(u8, u16)], i: &mut usize, res: &[bool]) -> bool {
        let Some(&(op, n)) = ops.get(*i) else { return true };
        *i += 1;
        match op {
            1 => res.get(n as usize).copied().unwrap_or(true),
            2 => { let mut all = true; for _ in 0..n { if !eval(ops, i, res) { all = false; } } all }
            3 => { let mut any = false; for _ in 0..n { if eval(ops, i, res) { any = true; } } any }
            4 => !eval(ops, i, res),
            _ => true,
        }
    }
    let mut i = 0;
    eval(ops, &mut i, res)
}

pub struct Graph { pub nodes: Vec<Node>, pub aux: Vec<Aux>, pub root: u32, pub schema: std::sync::Arc<Schema> }

fn keys_of(p: &Map<String, Value>, schema: &Schema) -> HashMap<String, u16> {
    let mut m = HashMap::new();
    for (k, v) in p { if let Some(s) = v.as_str() { if let Some(i) = schema.key(s) { m.insert(k.clone(), i); } } }
    m
}

impl Graph {
    pub fn build(bt: &Value, schema: std::sync::Arc<Schema>, root_tree: &str) -> Result<Graph, String> {
        let mut g = Graph { nodes: vec![], aux: vec![], root: 0, schema };
        let trees = bt["trees"].as_object().ok_or("no trees")?;
        let root = g.add_tree(trees, root_tree, None, 0)?;
        g.root = root;
        Ok(g)
    }
    fn add_aux(&mut self, tree: &str, n: &Value, kind: AuxKind) -> u32 {
        let class = n["c"].as_str().unwrap_or("").to_string();
        let p = n["p"].as_object().cloned().unwrap_or_default();
        let keys = keys_of(&p, &self.schema);
        let gf = |k: &str| p.get(k).and_then(|v| v.as_f64());
        let abort = Abort::from(p.get("FlowAbortMode").and_then(|v| v.as_i64()).unwrap_or(0));
        let deco = match class.as_str() {
            "BTDecorator_Blackboard" => {
                let kind = match p.get("OpKind").and_then(|v| v.as_str()) { Some("Basic") => OpKind::Basic, Some("Text") => OpKind::Text, _ => OpKind::Arithmetic };
                Deco::Blackboard { key: keys.get("BlackboardKey").copied(), kind, op: gf("OperationType").unwrap_or(0.) as i32, int: gf("IntValue").unwrap_or(0.) as i32, float: gf("FloatValue").unwrap_or(0.) as f32, inverse: p.get("bInverseCondition").and_then(|v| v.as_bool()).unwrap_or(false) }
            }
            "BTDecorator_Loop" => Deco::Loop { num: gf("NumLoops").unwrap_or(3.) as i32, infinite: p.get("bInfiniteLoop").and_then(|v| v.as_bool()).unwrap_or(false), timeout: gf("InfiniteLoopTimeoutTime").unwrap_or(-1.) as f32 },
            "BTDecorator_ForceSuccess" => Deco::ForceSuccess,
            _ => Deco::Custom,
        };
        let a = Aux { tree: tree.to_string(), name: n["name"].as_str().unwrap_or("").to_string(), interval: gf("Interval").unwrap_or(0.5) as f32, deviation: gf("RandomDeviation").unwrap_or(0.1) as f32, tick_on_search: p.get("bCallTickOnSearchStart").and_then(|v| v.as_bool()).unwrap_or(false), class, kind, keys, abort, deco, p };
        self.aux.push(a);
        (self.aux.len() - 1) as u32
    }
    fn add_tree(&mut self, trees: &Map<String, Value>, name: &str, parent: Option<u32>, depth: u32) -> Result<u32, String> {
        if depth > 16 { return Err(format!("subtree recursion at {name}")); }
        let t = trees.get(name).ok_or_else(|| format!("tree {name} missing"))?;
        let root = t["root"].as_i64().ok_or("no root")? as usize;
        self.add_node(trees, name, t, root, parent, depth)
    }
    fn add_node(&mut self, trees: &Map<String, Value>, tree: &str, t: &Value, idx: usize, parent: Option<u32>, depth: u32) -> Result<u32, String> {
        let n = &t["nodes"][idx];
        let class = n["c"].as_str().unwrap_or("").to_string();
        let p = n["p"].as_object().cloned().unwrap_or_default();
        let kind = match class.as_str() {
            "BTComposite_Selector" => Kind::Selector,
            "BTComposite_Sequence" => Kind::Sequence,
            "BTComposite_SimpleParallel" => Kind::Parallel,
            "BTTask_RunBehavior" => Kind::Subtree,
            _ => Kind::Task,
        };
        let id = self.nodes.len() as u32;
        let keys = keys_of(&p, &self.schema);
        self.nodes.push(Node { tree: tree.to_string(), name: n["name"].as_str().unwrap_or("").to_string(), class, kind, p, keys, children: vec![], services: vec![], parent });
        let services: Vec<u32> = n["s"].as_array().map(|s| s.iter().filter_map(|i| i.as_u64()).map(|i| self.add_aux(tree, &t["nodes"][i as usize], AuxKind::Service)).collect()).unwrap_or_default();
        let mut children = vec![];
        if kind == Kind::Subtree {
            let sub = self.nodes[id as usize].s("BehaviorAsset").unwrap_or("").to_string();
            let st = trees.get(&sub).ok_or_else(|| format!("subtree {sub} missing"))?;
            let decos: Vec<u32> = st["rootDecorators"].as_array().map(|d| d.iter().filter_map(|i| i.as_u64()).map(|i| self.add_aux(&sub, &st["nodes"][i as usize], AuxKind::Decorator)).collect()).unwrap_or_default();
            let c = self.add_tree(trees, &sub, Some(id), depth + 1)?;
            children.push(Child { node: c, decos, ops: vec![] });
        } else if let Some(ch) = n["ch"].as_array() {
            for c in ch {
                let decos: Vec<u32> = c["d"].as_array().map(|d| d.iter().filter_map(|i| i.as_u64()).map(|i| self.add_aux(tree, &t["nodes"][i as usize], AuxKind::Decorator)).collect()).unwrap_or_default();
                let ops: Vec<(u8, u16)> = c["ops"].as_array().map(|o| o.iter().map(|x| (x["Operation"].as_u64().unwrap_or(0) as u8, x["Number"].as_u64().unwrap_or(0) as u16)).collect()).unwrap_or_default();
                let Some(ci) = c["n"].as_u64() else { continue };
                let cn = self.add_node(trees, tree, t, ci as usize, Some(id), depth)?;
                children.push(Child { node: cn, decos, ops });
            }
        }
        let node = &mut self.nodes[id as usize];
        node.children = children;
        node.services = services;
        Ok(id)
    }
}

pub trait Host {
    fn bb(&self) -> &Blackboard;
    fn now(&self) -> f32;
    fn rand01(&mut self) -> f32;
    fn decorator(&mut self, g: &Graph, aux: u32) -> bool;
    fn service_activate(&mut self, g: &Graph, aux: u32);
    fn service_deactivate(&mut self, g: &Graph, aux: u32);
    fn service_tick(&mut self, g: &Graph, aux: u32, dt: f32);
    fn task_start(&mut self, g: &Graph, node: u32) -> Status;
    fn task_tick(&mut self, g: &Graph, node: u32, dt: f32) -> Status;
    fn task_abort(&mut self, g: &Graph, node: u32);
}

#[derive(Clone, Copy, Default)]
struct AuxMem { next: f32, last: f32, active: bool, observed: Option<bool>, loops: i32, loop_start: f32 }

pub struct Runner {
    path: Vec<(u32, usize)>,
    active: Option<u32>,
    aux: Vec<AuxMem>,
    live: Vec<u32>,
    run_forced: bool,
    pub steps: u64,
    pub trace: bool,
    pub log: Vec<String>,
}

enum Step { Search(usize), Enter(u32), Done(Status), CompDone(Status) }

const MAX_STEPS: usize = 256;

impl Runner {
    pub fn new(g: &Graph) -> Runner { Runner { path: vec![], active: None, aux: vec![AuxMem::default(); g.aux.len()], live: vec![], run_forced: false, steps: 0, trace: false, log: vec![] } }
    pub fn active_task(&self) -> Option<u32> { self.active }
    pub fn path(&self) -> &[(u32, usize)] { &self.path }
    pub fn live_services(&self) -> &[u32] { &self.live }

    pub fn check_deco(g: &Graph, h: &mut dyn Host, a: u32) -> bool {
        let aux = &g.aux[a as usize];
        match &aux.deco {
            Deco::Blackboard { key, kind, op, int, float, inverse } => {
                let Some(k) = key else { return !inverse };
                let bb = h.bb();
                let r = match kind {
                    OpKind::Basic => test_basic(bb.is_set(*k), *op),
                    OpKind::Arithmetic => { let v = bb.number(*k); let is_float = matches!(bb.get(*k), crate::bb::Val::Float(_)); test_arith(v, *op, if is_float { *float as f64 } else { *int as f64 }) }
                    OpKind::Text => false,
                };
                r != *inverse
            }
            Deco::Loop { .. } | Deco::ForceSuccess => true,
            Deco::Custom => { let inv = aux.p.get("bInverseCondition").and_then(|v| v.as_bool()).unwrap_or(false); h.decorator(g, a) != inv }
        }
    }

    fn decos_pass(&mut self, g: &Graph, h: &mut dyn Host, c: &Child) -> bool {
        let mut res = Vec::with_capacity(c.decos.len());
        for &d in &c.decos {
            let r = Self::check_deco(g, h, d);
            self.aux[d as usize].observed = Some(r);
            res.push(r);
        }
        combine(&c.ops, &res)
    }

    fn decos_now(g: &Graph, h: &mut dyn Host, c: &Child) -> bool {
        let res: Vec<bool> = c.decos.iter().map(|&d| Self::check_deco(g, h, d)).collect();
        combine(&c.ops, &res)
    }

    fn activate(&mut self, g: &Graph, h: &mut dyn Host, node: u32) {
        let now = h.now();
        for &s in &g.nodes[node as usize].services {
            if self.aux[s as usize].active { continue; }
            let a = &g.aux[s as usize];
            let dev = if a.deviation > 0. { (h.rand01() * 2. - 1.) * a.deviation } else { 0. };
            let m = &mut self.aux[s as usize];
            m.active = true; m.last = now; m.next = now + (a.interval + dev).max(0.);
            self.live.push(s);
            h.service_activate(g, s);
            if a.tick_on_search { h.service_tick(g, s, 0.); }
        }
    }

    fn deactivate(&mut self, g: &Graph, h: &mut dyn Host, node: u32) {
        for &s in &g.nodes[node as usize].services {
            if !self.aux[s as usize].active { continue; }
            self.aux[s as usize].active = false;
            self.live.retain(|x| *x != s);
            h.service_deactivate(g, s);
        }
    }

    fn deactivate_subtree(&mut self, g: &Graph, h: &mut dyn Host, node: u32) {
        self.deactivate(g, h, node);
        for c in g.nodes[node as usize].children.clone() { self.deactivate_subtree(g, h, c.node); }
    }

    fn activate_background(&mut self, g: &Graph, h: &mut dyn Host, node: u32) {
        self.activate(g, h, node);
        let n = &g.nodes[node as usize];
        if n.kind == Kind::Task { return; }
        let children = n.children.clone();
        for c in children { if self.decos_pass(g, h, &c) { self.activate_background(g, h, c.node); if n.kind != Kind::Parallel { break; } } }
    }

    fn abort_below(&mut self, g: &Graph, h: &mut dyn Host, depth: usize) {
        if let Some(t) = self.active.take() { h.task_abort(g, t); self.deactivate(g, h, t); }
        while self.path.len() > depth + 1 {
            let (n, _) = self.path.pop().unwrap();
            self.deactivate_subtree(g, h, n);
        }
        let (comp, ci) = self.path[depth];
        if let Some(c) = g.nodes[comp as usize].children.get(ci) { self.deactivate_subtree(g, h, c.node); }
    }

    pub fn tick(&mut self, g: &Graph, h: &mut dyn Host, dt: f32) {
        let now = h.now();
        if self.path.is_empty() && self.active.is_none() {
            self.activate(g, h, g.root);
            self.path.push((g.root, 0));
            self.run(g, h, Step::Search(0));
            return;
        }
        let live = self.live.clone();
        for s in live {
            let m = self.aux[s as usize];
            if !m.active || now < m.next { continue; }
            let a = &g.aux[s as usize];
            let dev = if a.deviation > 0. { (h.rand01() * 2. - 1.) * a.deviation } else { 0. };
            let since = now - m.last;
            { let m = &mut self.aux[s as usize]; m.last = now; m.next = now + (a.interval + dev).max(0.); }
            h.service_tick(g, s, since);
        }
        if let Some(step) = self.observe(g, h) { self.run(g, h, step); return; }
        if let Some(t) = self.active {
            match h.task_tick(g, t, dt) {
                Status::InProgress => {}
                r => { self.active = None; self.deactivate(g, h, t); self.run(g, h, Step::Done(r)); }
            }
        } else if !self.path.is_empty() {
            self.run(g, h, Step::Search(self.path.last().unwrap().1 + 1));
        }
    }

    fn observe(&mut self, g: &Graph, h: &mut dyn Host) -> Option<Step> {
        for d in 0..self.path.len() {
            let (comp, ci) = self.path[d];
            let node = &g.nodes[comp as usize];
            if node.kind == Kind::Parallel && ci > 0 { continue; }
            if let Some(c) = node.children.get(ci) {
                let observing = c.decos.iter().any(|&a| g.aux[a as usize].abort.observes_self());
                if observing {
                    let r = if c.ops.is_empty() { c.decos.iter().filter(|&&a| g.aux[a as usize].abort.observes_self()).all(|&a| Self::check_deco(g, h, a)) } else { Self::decos_now(g, h, c) };
                    if !r {
                        let a = c.decos[0];
                        if self.trace { self.log.push(format!("abort self {} by {}", g.nodes[c.node as usize].label(), g.aux[a as usize].label())); }
                        self.abort_below(g, h, d);
                        self.path.truncate(d + 1);
                        return Some(Step::Done(Status::Failed));
                    }
                }
            }
            if node.kind != Kind::Selector { continue; }
            for j in 0..ci.min(node.children.len()) {
                let c = &node.children[j];
                let mut changed = false;
                for &a in &c.decos {
                    if !g.aux[a as usize].abort.observes_lower() { continue; }
                    let r = Self::check_deco(g, h, a);
                    let prev = self.aux[a as usize].observed.replace(r);
                    if r && prev != Some(true) { changed = true; }
                }
                if !changed { continue; }
                let c = c.clone();
                if !self.decos_pass(g, h, &c) { continue; }
                if self.trace { self.log.push(format!("abort lower: restart at {}", g.nodes[c.node as usize].label())); }
                self.abort_below(g, h, d);
                self.path.truncate(d + 1);
                self.path[d].1 = j;
                self.start_loops(g, h.now(), &c);
                return Some(Step::Enter(c.node));
            }
        }
        None
    }

    fn start_loops(&mut self, g: &Graph, now: f32, c: &Child) {
        for &d in &c.decos { if let Deco::Loop { .. } = g.aux[d as usize].deco { let m = &mut self.aux[d as usize]; m.loops = 0; m.loop_start = now; } }
    }

    fn run(&mut self, g: &Graph, h: &mut dyn Host, mut step: Step) {
        for _ in 0..MAX_STEPS {
            self.steps += 1;
            step = match step {
                Step::Search(from) => {
                    let Some(&(comp, _)) = self.path.last() else { return };
                    let node = &g.nodes[comp as usize];
                    let n = node.children.len();
                    let mut next = None;
                    let mut forced = false;
                    for j in from..n {
                        let c = node.children[j].clone();
                        let ok = self.decos_pass(g, h, &c);
                        if ok { next = Some((j, c)); break; }
                        let force = c.decos.iter().any(|&d| matches!(g.aux[d as usize].deco, Deco::ForceSuccess));
                        if force && node.kind == Kind::Selector { forced = true; break; }
                        if force { continue; }
                        if node.kind != Kind::Selector { break; }
                    }
                    if forced { self.run_forced = true; }
                    match next {
                        Some((j, c)) => { self.path.last_mut().unwrap().1 = j; self.start_loops(g, h.now(), &c); Step::Enter(c.node) }
                        None => {
                            let forced = std::mem::take(&mut self.run_forced);
                            let failed_child = node.children[from.min(n)..].iter().any(|c| !c.decos.iter().any(|&d| matches!(g.aux[d as usize].deco, Deco::ForceSuccess)));
                            let r = match node.kind { Kind::Selector => if forced { Status::Succeeded } else { Status::Failed }, Kind::Sequence => if from < n && failed_child { Status::Failed } else { Status::Succeeded }, _ => Status::Failed };
                            Step::CompDone(r)
                        }
                    }
                }
                Step::Enter(id) => {
                    let node = &g.nodes[id as usize];
                    if self.trace { self.log.push(format!("enter {}", node.label())); }
                    self.activate(g, h, id);
                    match node.kind {
                        Kind::Task => match h.task_start(g, id) {
                            Status::InProgress => { self.active = Some(id); return; }
                            r => { self.deactivate(g, h, id); Step::Done(r) }
                        },
                        Kind::Parallel => {
                            self.path.push((id, 0));
                            let kids = node.children.clone();
                            for c in kids.iter().skip(1) { if self.decos_pass(g, h, c) { self.activate_background(g, h, c.node); } }
                            match kids.first() { Some(c) if self.decos_pass(g, h, c) => Step::Enter(c.node), _ => Step::CompDone(Status::Failed) }
                        }
                        _ => { self.path.push((id, 0)); Step::Search(0) }
                    }
                }
                Step::Done(mut r) => {
                    let Some(&(comp, ci)) = self.path.last() else { return };
                    let node = &g.nodes[comp as usize];
                    let Some(c) = node.children.get(ci).cloned() else { return };
                    let mut again = false;
                    for &d in &c.decos {
                        match g.aux[d as usize].deco {
                            Deco::Loop { num, infinite, timeout } if r != Status::Failed => {
                                let now = h.now();
                                let m = &mut self.aux[d as usize];
                                m.loops += 1;
                                let more = if infinite { timeout < 0. || now - m.loop_start < timeout } else { m.loops < num };
                                if more { again = true; }
                            }
                            Deco::ForceSuccess => r = Status::Succeeded,
                            _ => {}
                        }
                    }
                    if again && self.decos_pass(g, h, &c) {
                        Step::Enter(c.node)
                    } else {
                        match node.kind {
                            Kind::Selector => if r == Status::Succeeded { Step::CompDone(r) } else { Step::Search(ci + 1) },
                            Kind::Sequence => if r == Status::Failed { Step::CompDone(r) } else { Step::Search(ci + 1) },
                            _ => Step::CompDone(r),
                        }
                    }
                }
                Step::CompDone(r) => {
                    let Some((comp, _)) = self.path.pop() else { return };
                    self.deactivate_subtree(g, h, comp);
                    if self.trace { self.log.push(format!("done {} {:?}", g.nodes[comp as usize].label(), r)); }
                    if self.path.is_empty() { return; }
                    Step::Done(r)
                }
            };
        }
    }
}
