use crate::pickups_dxbc::Dxbc;
use fn_fx::view::ViewParams;
use fn_particles::assets::Catalog;
use fn_particles::{Env, Instance, Params, Transform};
use fn_render::fx::FxRenderer;
use glam::{Mat3, Quat, Vec3};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const P_WATERFALL: &str = "/Game/Effects/Fort_Effects/Effects/Water/P_WaterFall_Base";

struct Spot { level: String, t: Vec3, q: Quat, s: Vec3, cull_cm: f32, params: Params, inst: Option<Instance> }
struct State { catalog: Option<Arc<Catalog>>, warmed: bool, errors: std::collections::HashSet<String> }

static SPOTS: Mutex<Vec<Spot>> = Mutex::new(Vec::new());
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn v3(v: &Value) -> Option<Vec3> { let a = v.as_array()?; Some(Vec3::new(a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32)) }

pub fn ingest_poi(doc: &Value) {
    let Some(ps) = doc["particles"].as_array() else { return };
    let level = doc["level"].as_str().unwrap_or("").to_owned();
    let mut add = vec![];
    for p in ps {
        if p["template"].as_str().map(fn_uasset_leafless) != Some(P_WATERFALL) { continue; }
        if p["auto_activate"].as_bool() == Some(false) || p["hidden"].as_bool() == Some(true) { continue; }
        let (Some(t), Some(s)) = (v3(&p["t"]), v3(&p["s"])) else { continue };
        let q = p["q"].as_array().map(|a| Quat::from_xyzw(a[0].as_f64().unwrap_or(0.) as f32, a[1].as_f64().unwrap_or(0.) as f32, a[2].as_f64().unwrap_or(0.) as f32, a[3].as_f64().unwrap_or(1.) as f32)).unwrap_or(Quat::IDENTITY).normalize();
        let mut params = Params::default();
        for ip in p["params"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            let name = ip["Name"].as_str().unwrap_or("");
            match ip["ParamType"].as_str() {
                Some("PSPT_Vector") => if let Some(v) = v3(&ip["Vector"]) { params.set_vector(name, v.to_array()); },
                Some("PSPT_Scalar") => params.set_float(name, ip["Scalar"].as_f64().unwrap_or(0.) as f32),
                _ => {}
            }
        }
        add.push(Spot { level: level.clone(), t, q, s, cull_cm: p["cull_cm"].as_f64().unwrap_or(0.) as f32, params, inst: None });
    }
    if let Ok(mut g) = SPOTS.lock() { g.retain(|x| x.level != level); }
    if add.is_empty() { return; }
    if let Ok(mut g) = SPOTS.lock() { eprintln!("[waterfall] {} active WaterFallVFX ({} so far)", add.len(), g.len() + add.len()); g.extend(add); }
}

fn fn_uasset_leafless(p: &str) -> &str { p.split('.').next().unwrap_or(p) }

pub fn init(roots: &[PathBuf]) {
    let dir = roots.iter().map(|r| r.join("particles-waterfall")).find(|d| d.join("index.json").is_file());
    let catalog = match dir.as_ref().map(|d| Catalog::load(d)) {
        Some(Ok(c)) if c.system(P_WATERFALL).is_some() => { eprintln!("[waterfall] P_WaterFall_Base loaded from {}", dir.as_ref().unwrap().display()); Some(Arc::new(c)) }
        Some(Ok(_)) => { eprintln!("[waterfall] the particles-waterfall catalog lacks P_WaterFall_Base: no waterfall foam"); None }
        Some(Err(e)) => { eprintln!("[waterfall] {e}: no waterfall foam"); None }
        None => { eprintln!("[waterfall] no `particles-waterfall` folder in the asset roots (fn-particles-export --out <assets>/particles-waterfall {P_WATERFALL}): no waterfall foam"); None }
    };
    *STATE.lock().unwrap() = Some(State { catalog, warmed: false, errors: Default::default() });
}

fn draw(dx: &mut Dxbc, fx: &mut FxRenderer, cat: &Catalog, inst: &Instance, eye: [f32; 3], dt: f32, errors: &mut std::collections::HashSet<String>) {
    let mut d = vec![];
    inst.draw(eye, &mut d);
    let bounds = crate::particles::instance_bounds(inst, cat);
    for e in &d {
        if let Err(err) = dx.draw_emitter(fx, e, &inst.xf, &bounds, dt) {
            let k = format!("{}/{}", e.system, e.emitter);
            if errors.insert(k.clone()) { eprintln!("[waterfall] {k}: NOT DRAWN: {err}"); }
        }
    }
}

pub fn warm(dx: &mut Dxbc, fx: &mut FxRenderer) {
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    if st.warmed { return; }
    st.warmed = true;
    let Some(cat) = st.catalog.clone() else { return };
    let Some(s) = cat.system(P_WATERFALL) else { return };
    let t = std::time::Instant::now();
    let mut i = Instance::new(s, Transform::identity(), 1, 100.);
    for _ in 0..30 { i.tick(1. / 30., &Env::none()); }
    draw(dx, fx, &cat, &i, [0., 2., 3.], 1. / 60., &mut st.errors);
    eprintln!("[waterfall] warm-up: foam materials / meshes built in {:.0} ms", t.elapsed().as_secs_f32() * 1000.);
}

pub fn tick(view: &ViewParams, dt: f32) -> bool {
    let Ok(guard) = STATE.lock() else { return false };
    let Some(cat) = guard.as_ref().and_then(|s| s.catalog.clone()) else { return false };
    drop(guard);
    let Some(sys) = cat.system(P_WATERFALL) else { return false };
    let Some(off) = crate::poi::current().map(|s| s.off) else { return false };
    let Ok(mut spots) = SPOTS.lock() else { return false };
    let eye_fx = fn_particles::sim::game_to_ue([view.eye.x, view.eye.y, view.eye.z]);
    let env_ground = |x: f32, y: f32| fn_terrain::ground_y(x * 0.01, y * 0.01) * 100.;
    let env = Env { ground: Some(&env_ground) };
    let mut any = false;
    for (k, sp) in spots.iter_mut().enumerate() {
        let pos = off.transform_point3(sp.t);
        let dist = (pos - eye_fx).length();
        if sp.cull_cm > 0. && dist > sp.cull_cm { sp.inst = None; continue; }
        let scale = (sp.s.x * sp.s.y * sp.s.z).abs().cbrt();
        let xf = Transform { pos, rot: Mat3::from_quat(sp.q), scale };
        let inst = sp.inst.get_or_insert_with(|| Instance::new_with_params(sys.clone(), xf, 0x5746_0000 ^ k as u64, dist, &sp.params));
        inst.xf = xf;
        inst.tick(dt, &env);
        any = true;
    }
    any
}

pub fn draw_into(dx: &mut Dxbc, fx: &mut FxRenderer, view: &ViewParams, dt: f32) {
    let Ok(mut guard) = STATE.lock() else { return };
    let Some(st) = guard.as_mut() else { return };
    let Some(cat) = st.catalog.clone() else { return };
    let Ok(spots) = SPOTS.lock() else { return };
    let eye = [view.eye.x, view.eye.y, view.eye.z];
    for sp in spots.iter() { if let Some(i) = sp.inst.as_ref() { draw(dx, fx, &cat, i, eye, dt, &mut st.errors); } }
}
