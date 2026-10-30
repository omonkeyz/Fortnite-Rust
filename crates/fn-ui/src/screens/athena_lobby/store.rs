//! Package store for the lobby UI: cooked packages of the 1.8 install decoded into plain JSON property trees (the same data `fn-inspect props`
//! prints), Blueprint class-default resolution through the parent chain, and the `Game.locres` string table.
//!
//! Nothing is cached on disk and nothing is run: packages are read from the paks on demand and kept in memory.
use fn_pak::PakSet;
use fn_uasset::PackageData;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

/// One export of a package: decoded tagged properties plus its place in the object hierarchy.
pub struct Exp {
    pub name: String,
    pub class: String,
    /// Full object path: `/Game/Pkg.Name` for top-level objects, `outer:Name` for nested ones (the form widget / slot references use).
    pub path: String,
    pub outer: String,
    /// Parent class path (class exports only; `/Script/Module.Class` for native parents).
    pub super_path: String,
    pub props: Value,
}

pub struct Pkg {
    pub name: String,
    pub exports: Vec<Exp>,
    pub by_path: HashMap<String, usize>,
    /// outer path -> export indices
    pub by_outer: HashMap<String, Vec<usize>>,
    pub by_name: HashMap<String, usize>,
}
impl Pkg {
    pub fn get(&self, path: &str) -> Option<&Exp> { self.by_path.get(path).map(|&i| &self.exports[i]) }
    pub fn named(&self, name: &str) -> Option<&Exp> { self.by_name.get(name).map(|&i| &self.exports[i]) }
    pub fn children_of(&self, outer: &str) -> impl Iterator<Item = &Exp> + '_ {
        self.by_outer.get(outer).into_iter().flat_map(|v| v.iter().map(|&i| &self.exports[i]))
    }
}

pub static IS_MAIN_THREAD: std::sync::OnceLock<fn() -> bool> = std::sync::OnceLock::new();
static PKG_PRELOADED: std::sync::Mutex<Option<HashMap<String, Option<Pkg>>>> = std::sync::Mutex::new(None);
static PKG_QUEUE: std::sync::Mutex<std::collections::VecDeque<String>> = std::sync::Mutex::new(std::collections::VecDeque::new());
pub fn preload(install: std::path::PathBuf, list: Vec<String>, threads: usize) {
    if let Ok(mut q) = PKG_QUEUE.lock() { q.extend(list); }
    for _ in 0..threads.max(1) {
        let dir = install.clone();
        let _ = std::thread::Builder::new().name("ui package preload".into()).spawn(move || {
            let Ok(set) = PakSet::open_install(&dir) else { return };
            loop {
                let Some(long) = PKG_QUEUE.lock().ok().and_then(|mut q| q.pop_front()) else { break };
                if PKG_PRELOADED.lock().map_or(true, |g| g.as_ref().is_some_and(|m| m.contains_key(&long))) { continue; }
                let p = load_pkg(&set, &long);
                if let Ok(mut g) = PKG_PRELOADED.lock() { g.get_or_insert_with(HashMap::new).insert(long, p); }
            }
        });
    }
}

pub struct Store {
    set: PakSet,
    pkgs: RefCell<HashMap<String, Option<Rc<Pkg>>>>,
    locres: RefCell<Option<Rc<Locres>>>,
    fns: RefCell<HashMap<String, Rc<HashMap<String, (Rc<Vec<fn_uasset::kismet::Statement>>, Vec<String>)>>>>,
}

/// `/Game/Foo/Bar.Bar_C` -> (`/Game/Foo/Bar`, `Bar_C`).
pub fn split_object_path(p: &str) -> (&str, &str) {
    match p.split_once('.') { Some((a, b)) => (a, b.split(':').next().unwrap_or(b)), None => (p, "") }
}

impl Store {
    pub fn open(install: &Path) -> Result<Store, String> {
        let set = PakSet::open_install(install).map_err(|e| format!("cannot open the paks under {}: {e}", install.display()))?;
        Ok(Store { set, pkgs: RefCell::new(HashMap::new()), locres: RefCell::new(None), fns: Default::default() })
    }
    pub fn from_set(set: PakSet) -> Store { Store { set, pkgs: RefCell::new(HashMap::new()), locres: RefCell::new(None), fns: Default::default() } }
    pub fn pak_set(&self) -> &PakSet { &self.set }

    /// Decode a package (`/Game/UI/Foo/Bar`; a trailing `.Object` is ignored). `None` when the package is not in the paks (script / engine packages).
    pub fn pkg(&self, long: &str) -> Option<Rc<Pkg>> {
        let long = long.split('.').next().unwrap_or(long);
        if let Some(c) = self.pkgs.borrow().get(long) { return c.clone(); }
        let loaded = self.load(long).map(Rc::new);
        self.pkgs.borrow_mut().insert(long.to_owned(), loaded.clone());
        loaded
    }

    fn load(&self, long: &str) -> Option<Pkg> {
        if let Some(p) = PKG_PRELOADED.lock().ok().and_then(|mut g| g.as_mut().and_then(|m| m.remove(long))) { return p; }
        let main = IS_MAIN_THREAD.get().is_some_and(|f| f());
        let t = std::time::Instant::now();
        let r = load_pkg(&self.set, long);
        let ms = t.elapsed().as_secs_f32() * 1000.;
        if main && ms >= 1. { eprintln!("[pkgrec] {ms:.1} {long}"); }
        r
    }
}

fn load_pkg(set: &PakSet, long: &str) -> Option<Pkg> {
        if !long.starts_with("/Game/") && !long.starts_with("/Engine/") { return None; }
        let data = PackageData::load(set, long).ok()?;
        let pkg = data.package().ok()?;
        let mut exports = Vec::with_capacity(pkg.exports.len());
        for (i, e) in pkg.exports.iter().enumerate() {
            let class = pkg.export_class_name(i);
            let outer = pkg.object_path(e.outer_index);
            let path = if outer.is_empty() { format!("{long}.{}", e.name) } else { format!("{outer}:{}", e.name) };
            let super_path = if e.super_index.is_null() { String::new() } else { pkg.object_path(e.super_index) };
            let mut props = match pkg.read_export(i) {
                Ok(o) => o.to_json(&pkg).get("properties").cloned().unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            // DataTable rows live in the native payload: expose them as props["Rows"][row] (e.g. InputActionDataTable)
            if class == "DataTable" {
                if let Ok(t) = fn_uasset::classes::read_table(&pkg, i) {
                    let mut m = serde_json::Map::new();
                    for (n, p) in &t.rows { m.insert(n.clone(), p.to_json(&pkg)); }
                    if !props.is_object() { props = Value::Object(Default::default()); }
                    props["Rows"] = Value::Object(m);
                }
            }
            exports.push(Exp { name: e.name.clone(), class, path, outer, super_path, props });
        }
        let mut by_path = HashMap::new();
        let mut by_outer: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_name = HashMap::new();
        for (i, e) in exports.iter().enumerate() {
            by_path.insert(e.path.clone(), i);
            by_outer.entry(e.outer.clone()).or_default().push(i);
            by_name.entry(e.name.clone()).or_insert(i);
        }
        Some(Pkg { name: long.to_owned(), exports, by_path, by_outer, by_name })
    }

impl Store {

    /// Every Blueprint function of package `long` decoded once: (statements, input parameter names), by function name.
    fn package_functions(&self, long: &str) -> Rc<HashMap<String, (Rc<Vec<fn_uasset::kismet::Statement>>, Vec<String>)>> {
        let long = long.split('.').next().unwrap_or(long);
        if let Some(c) = self.fns.borrow().get(long) { return c.clone(); }
        let mut out = HashMap::new();
        if let Ok(data) = PackageData::load(&self.set, long) {
            if let Ok(pkg) = data.package() {
                for i in 0..pkg.exports.len() {
                    if pkg.export_class_name(i) != "Function" { continue; }
                    let Ok(f) = fn_uasset::classes::read_function(&pkg, i) else { continue };
                    let stmts = Rc::new(fn_uasset::kismet::disassemble(&pkg, f.script).statements);
                    // input parameters: the `children` chain, CPF_Parm 0x80 without CPF_OutParm 0x100 / CPF_ReturnParm 0x400
                    let mut params = vec![];
                    let mut cur = f.children.export_index();
                    while let Some(ci) = cur {
                        let Ok(d) = fn_uasset::classes::read_property_decl(&pkg, ci) else { break };
                        if d.property_flags & 0x80 != 0 && d.property_flags & 0x500 == 0 { params.push(d.name.clone()); }
                        cur = d.next.export_index();
                    }
                    out.insert(pkg.exports[i].name.clone(), (stmts, params));
                }
            }
        }
        let rc = Rc::new(out);
        self.fns.borrow_mut().insert(long.to_owned(), rc.clone());
        rc
    }

    /// Kismet statements of the Blueprint function `name` of package `long` (`/Game/UI/Foo/Bar`), decoded from the cooked bytecode (`fn_uasset::kismet`).
    pub fn function(&self, long: &str, name: &str) -> Option<Rc<Vec<fn_uasset::kismet::Statement>>> { self.package_functions(long).get(name).map(|f| f.0.clone()) }

    /// Input parameter names of the Blueprint function `name` (in declaration order; out / return parameters excluded).
    pub fn function_params(&self, long: &str, name: &str) -> Vec<String> { self.package_functions(long).get(name).map(|f| f.1.clone()).unwrap_or_default() }

    /// Names of the Blueprint functions of package `long` (research / discovery helper).
    pub fn function_names(&self, long: &str) -> Vec<String> { let mut v: Vec<String> = self.package_functions(long).keys().cloned().collect(); v.sort(); v }
    /// The export an object path (`/Game/Pkg.Name`, `/Game/Pkg.Outer:Name`) names.
    pub fn object(&self, path: &str) -> Option<(Rc<Pkg>, usize)> {
        let (long, _) = split_object_path(path);
        let pkg = self.pkg(long)?;
        if let Some(&i) = pkg.by_path.get(path) { return Some((pkg, i)); }
        let name = path.rsplit([':', '.']).next()?;
        let i = *pkg.by_name.get(name)?;
        Some((pkg, i))
    }

    /// Class of the object an asset path names (`Texture2D`, `Material`, `MaterialInstanceConstant`, ...).
    pub fn object_class(&self, path: &str) -> Option<String> { self.object(path).map(|(p, i)| p.exports[i].class.clone()) }

    /// Parent chain of a Blueprint class, most derived first (`/Game/..X.X_C`, parent, ..., `/Script/..`).
    pub fn class_chain(&self, class_path: &str) -> Vec<String> {
        let mut out = vec![];
        let mut cur = class_path.to_owned();
        for _ in 0..32 {
            out.push(cur.clone());
            let Some((pkg, i)) = self.object(&cur) else { break };
            let sp = pkg.exports[i].super_path.clone();
            if sp.is_empty() || !sp.starts_with("/Game/") { if !sp.is_empty() { out.push(sp); } break; }
            cur = sp;
        }
        out
    }

    /// Effective class-default properties of a Blueprint class: the CDO of every class of the chain, base first, struct values merged one level
    /// deep (a CDO only stores what differs from its parent).
    pub fn class_defaults(&self, class_path: &str) -> Value {
        let chain = self.class_chain(class_path);
        let mut merged = serde_json::Map::new();
        for c in chain.iter().rev() {
            if !c.starts_with("/Game/") { continue; }
            let (long, name) = split_object_path(c);
            let Some(pkg) = self.pkg(long) else { continue };
            let Some(cdo) = pkg.named(&format!("Default__{name}")) else { continue };
            if let Some(o) = cdo.props.as_object() {
                for (k, v) in o { merge_prop(&mut merged, k, v); }
            }
        }
        Value::Object(merged)
    }

    /// `Game.locres` (English).
    pub fn locres(&self) -> Option<Rc<Locres>> {
        if let Some(l) = self.locres.borrow().as_ref() { return Some(l.clone()); }
        let bytes = self.set.read("FortniteGame/Content/Localization/Game/en/Game.locres").ok()?.into_owned();
        let l = Rc::new(Locres::parse(&bytes)?);
        *self.locres.borrow_mut() = Some(l.clone());
        Some(l)
    }
}

fn merge_prop(into: &mut serde_json::Map<String, Value>, k: &str, v: &Value) {
    match (into.get_mut(k), v) {
        (Some(Value::Object(a)), Value::Object(b)) => { for (kk, vv) in b { a.insert(kk.clone(), vv.clone()); } }
        _ => { into.insert(k.to_owned(), v.clone()); }
    }
}

/// UE 4.16 legacy `.locres`: `u32 namespaces { FString ns, u32 keys { FString key, u32 source hash, FString text } }`.
pub struct Locres { map: HashMap<(String, String), String> }
impl Locres {
    pub fn parse(b: &[u8]) -> Option<Locres> {
        let mut o = 0usize;
        fn u32_(b: &[u8], o: &mut usize) -> Option<u32> { let v = u32::from_le_bytes(b.get(*o..*o + 4)?.try_into().ok()?); *o += 4; Some(v) }
        fn fstr(b: &[u8], o: &mut usize) -> Option<String> {
            let n = i32::from_le_bytes(b.get(*o..*o + 4)?.try_into().ok()?); *o += 4;
            if n == 0 { return Some(String::new()); }
            if n < 0 {
                let len = (-n) as usize;
                let raw = b.get(*o..*o + len * 2)?; *o += len * 2;
                let u: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                Some(String::from_utf16_lossy(&u[..u.len().saturating_sub(1)]))
            } else {
                let len = n as usize;
                let raw = b.get(*o..*o + len)?; *o += len;
                Some(String::from_utf8_lossy(&raw[..raw.len().saturating_sub(1)]).into_owned())
            }
        }
        let nns = u32_(b, &mut o)?;
        let mut map = HashMap::new();
        for _ in 0..nns {
            let ns = fstr(b, &mut o)?;
            let nk = u32_(b, &mut o)?;
            for _ in 0..nk {
                let key = fstr(b, &mut o)?;
                let _hash = u32_(b, &mut o)?;
                let val = fstr(b, &mut o)?;
                map.insert((ns.clone(), key), val);
            }
        }
        Some(Locres { map })
    }
    pub fn get(&self, namespace: &str, key: &str) -> Option<&str> { self.map.get(&(namespace.to_owned(), key.to_owned())).map(|s| s.as_str()) }
    pub fn len(&self) -> usize { self.map.len() }
    /// Every (key, text) of one namespace, sorted by key (research helper).
    pub fn namespace(&self, ns: &str) -> Vec<(String, String)> { let mut v: Vec<(String, String)> = self.map.iter().filter(|(k, _)| k.0 == ns).map(|(k, v)| (k.1.clone(), v.clone())).collect(); v.sort(); v }
    /// (namespace, key) of every entry whose text equals `text` (research helper)
    /// Research helper: every entry whose text contains `sub` (`(namespace, key, text)`).
    pub fn find_containing(&self, sub: &str) -> Vec<(String, String, String)> { self.map.iter().filter(|(_, v)| v.contains(sub)).map(|(k, v)| (k.0.clone(), k.1.clone(), v.clone())).collect() }
    pub fn find_text(&self, text: &str) -> Vec<(String, String)> { self.map.iter().filter(|(_, v)| v.as_str() == text).map(|(k, _)| k.clone()).collect() }
}

// ---- small JSON accessors used across the module -------------------------------------------------------------------------------------------

pub fn jf(v: &Value) -> Option<f32> { v.as_f64().map(|x| x as f32) }
pub fn jvec<const N: usize>(v: Option<&Value>) -> Option<[f32; N]> {
    let a = v?.as_array()?;
    let mut out = [0f32; N];
    for i in 0..N { out[i] = a.get(i).and_then(jf).unwrap_or(0.0); }
    Some(out)
}
/// UMG `Margin` struct (missing keys are 0): (left, top, right, bottom).
pub fn jmargin(v: Option<&Value>) -> [f32; 4] {
    let Some(o) = v.and_then(|x| x.as_object()) else { return [0.0; 4] };
    let g = |k: &str| o.get(k).and_then(jf).unwrap_or(0.0);
    [g("Left"), g("Top"), g("Right"), g("Bottom")]
}
/// `ESomething::Value` -> `Value`.
pub fn jenum(v: Option<&Value>) -> Option<&str> { v.and_then(|x| x.as_str()).map(|s| s.rsplit("::").next().unwrap_or(s)) }

