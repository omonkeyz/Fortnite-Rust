use fn_render::local_lights::LocalLight;
use fn_ueio::props::{self, PropList, PropValue};
use fn_ueio::{Package, Reader};
use glam::{Quat, Vec3};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

static EFFECTS_QUALITY: AtomicU8 = AtomicU8::new(3);
pub fn set_effects_quality(level: u8) { EFFECTS_QUALITY.store(level, Ordering::Relaxed); }
pub fn effects_quality() -> u8 { EFFECTS_QUALITY.load(Ordering::Relaxed) }
pub fn detail_mode(effects: u8) -> u8 { [0u8, 1, 1, 2][effects.min(3) as usize] }

const CAR_PARENT: &str = "Car_Copper";
const CAR_HASH_OFFSET: [f32; 3] = [114.2469, 82.7856, 45.5213];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LightProps {
    pub intensity: Option<f32>,
    pub color: Option<[u8; 3]>,
    pub attenuation_radius: Option<f32>,
    pub falloff_exponent: Option<f32>,
    pub inverse_squared: Option<bool>,
    pub source_radius: Option<f32>,
    pub source_length: Option<f32>,
    pub inner_cone: Option<f32>,
    pub outer_cone: Option<f32>,
    pub cast_shadows: Option<bool>,
    pub max_draw_distance: Option<f32>,
    pub fade_range: Option<f32>,
    pub ies: Option<String>,
    pub detail_mode: Option<u8>,
    pub visible: Option<bool>,
    pub hidden_in_game: Option<bool>,
    pub affects_world: Option<bool>,
    pub min_roughness: Option<f32>,
    pub use_temperature: Option<bool>,
}

fn detail_of(s: &str) -> Option<u8> { match s.rsplit("::").next().unwrap_or(s) { "DM_Low" => Some(0), "DM_Medium" => Some(1), "DM_High" => Some(2), _ => None } }

impl LightProps {
    pub fn from_json(p: &Value) -> LightProps {
        let f = |k: &str| p.get(k).and_then(|v| v.as_f64()).map(|v| v as f32);
        let b = |k: &str| p.get(k).and_then(|v| v.as_bool());
        LightProps {
            intensity: f("Intensity"),
            color: p.get("LightColor").and_then(|c| Some([c.get("r")?.as_u64()? as u8, c.get("g")?.as_u64()? as u8, c.get("b")?.as_u64()? as u8])),
            attenuation_radius: f("AttenuationRadius"), falloff_exponent: f("LightFalloffExponent"), inverse_squared: b("bUseInverseSquaredFalloff"),
            source_radius: f("SourceRadius"), source_length: f("SourceLength"), inner_cone: f("InnerConeAngle"), outer_cone: f("OuterConeAngle"),
            cast_shadows: b("CastShadows"), max_draw_distance: f("MaxDrawDistance"), fade_range: f("MaxDistanceFadeRange"),
            ies: p.get("IESTexture").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned),
            detail_mode: p.get("DetailMode").and_then(|v| v.as_str()).and_then(detail_of),
            visible: b("bVisible"), hidden_in_game: b("bHiddenInGame"), affects_world: b("bAffectsWorld"), min_roughness: f("MinRoughness"), use_temperature: b("bUseTemperature"),
        }
    }
    pub fn from_props(pkg: &Package, p: &PropList) -> LightProps {
        let f = |k: &str| p.float(k).map(|v| v as f32);
        let ies = match p.get("IESTexture") { Some(PropValue::Object(i)) if *i != 0 => { let path = if *i < 0 { pkg.import_path(*i) } else { pkg.index_name(*i) }; (!path.is_empty()).then_some(path) } _ => None };
        LightProps {
            intensity: f("Intensity"),
            color: match p.get("LightColor") { Some(PropValue::Color(c)) => Some([c[2], c[1], c[0]]), _ => None },
            attenuation_radius: f("AttenuationRadius"), falloff_exponent: f("LightFalloffExponent"), inverse_squared: p.bool("bUseInverseSquaredFalloff"),
            source_radius: f("SourceRadius"), source_length: f("SourceLength"), inner_cone: f("InnerConeAngle"), outer_cone: f("OuterConeAngle"),
            cast_shadows: p.bool("CastShadows"), max_draw_distance: f("MaxDrawDistance"), fade_range: f("MaxDistanceFadeRange"), ies,
            detail_mode: match p.get("DetailMode") { Some(PropValue::Enum(s)) | Some(PropValue::Name(s)) => detail_of(s), Some(PropValue::Byte(b)) => Some(*b), _ => None },
            visible: p.bool("bVisible"), hidden_in_game: p.bool("bHiddenInGame"), affects_world: p.bool("bAffectsWorld"), min_roughness: f("MinRoughness"), use_temperature: p.bool("bUseTemperature"),
        }
    }
    pub fn overlay(&self, over: &LightProps) -> LightProps {
        macro_rules! pick { ($($f:ident),*) => { LightProps { $($f: over.$f.clone().or(self.$f.clone()),)* } } }
        pick!(intensity, color, attenuation_radius, falloff_exponent, inverse_squared, source_radius, source_length, inner_cone, outer_cone, cast_shadows, max_draw_distance, fade_range, ies, detail_mode, visible, hidden_in_game, affects_world, min_roughness, use_temperature)
    }
}

pub fn srgb_to_linear(c: u8) -> f32 { let c = c as f32 / 255.; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } }

pub struct IesProfile { pub row: u32, pub multiplier: f32 }

pub fn build_light(spot: bool, p: &LightProps, t_fx: Vec3, q: Quat, ies: Option<&IesProfile>, intensity_override: Option<f32>) -> LocalLight {
    use crate::lobby_world::point_defaults as d;
    let intensity = intensity_override.or(p.intensity).unwrap_or(d::INTENSITY).max(0.);
    let brightness = intensity * ies.map_or(1., |i| i.multiplier);
    let c = p.color.unwrap_or([255, 255, 255]);
    let fwd = q * Vec3::X;
    LocalLight {
        position_m: Vec3::new(t_fx.x, t_fx.z, t_fx.y) * 0.01,
        forward: Vec3::new(fwd.x, fwd.z, fwd.y).normalize_or_zero(),
        color: [srgb_to_linear(c[0]) * brightness, srgb_to_linear(c[1]) * brightness, srgb_to_linear(c[2]) * brightness],
        attenuation_radius_cm: p.attenuation_radius.unwrap_or(d::ATTENUATION_RADIUS),
        falloff_exponent: p.falloff_exponent.unwrap_or(d::FALLOFF_EXPONENT).max(1e-4),
        inverse_squared: p.inverse_squared.unwrap_or(true),
        spot_cone_deg: spot.then(|| (p.inner_cone.unwrap_or(d::SPOT_INNER_CONE_DEG), p.outer_cone.unwrap_or(d::SPOT_OUTER_CONE_DEG))),
        source_radius_cm: p.source_radius.unwrap_or(d::SOURCE_RADIUS),
        source_length_cm: p.source_length.unwrap_or(d::SOURCE_LENGTH),
        ies_row: ies.map(|i| i.row),
        cast_shadows: p.cast_shadows.unwrap_or(true),
        max_draw_distance_cm: p.max_draw_distance.unwrap_or(0.),
        fade_range_cm: p.fade_range.unwrap_or(0.),
        min_roughness: p.min_roughness.unwrap_or(fn_render::local_lights::LOCAL_LIGHT_MIN_ROUGHNESS),
    }
}

fn material_editor_sine(look_up: f32, period: f32) -> f32 { ((look_up * 360. / period) * (std::f32::consts::PI / 180.)).sin() }
pub fn car_location_hash(loc: [f32; 3]) -> f32 {
    let r = 5.59614f32;
    let v = [(loc[0] + 5.25) / r, (loc[1] + 5.25) / r];
    let fx = (v[0].ceil() as i32) as f32 / r;
    let fy = (v[1].ceil() as i32) as f32 / r;
    let a = fx * 0.04523;
    let s1 = fx + fy;
    let s2 = fy + a;
    let s3 = fx + fy * -0.21458;
    let sin1 = material_editor_sine(s3, 0.20184);
    let sin2 = material_editor_sine(sin1 * 0.023 + s1, 0.14587);
    let sin3 = material_editor_sine(s2 + sin2 * 0.01914, 0.1);
    let x = sin3 * sin2 * 13.11458;
    (x - x.trunc()).abs()
}
pub fn car_lights_on(actor_ue: [f32; 3], chance_left_off: f32) -> bool {
    car_location_hash([actor_ue[0] + CAR_HASH_OFFSET[0], actor_ue[1] + CAR_HASH_OFFSET[1], actor_ue[2] + CAR_HASH_OFFSET[2]]) > chance_left_off
}

#[derive(Clone, Debug, Default)]
struct CarClass { randomly_on: bool, head_lights_on: bool, head: f32, tail: f32, chance_off: f32 }

#[derive(Clone, Copy, Debug, PartialEq)]
enum Rule { Always, Chest }

struct Entry { actor: u32, rule: Rule, detail: u8, light: LocalLight }

struct State { entries: Vec<Entry>, last: Vec<usize>, notes: Vec<String>, prop_of: HashMap<u32, usize>, props_len: usize }
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn read_package(set: &fn_pak::PakSet, long: &str) -> Result<Package, String> {
    let read = |ext| set.read_game(long, ext).map(|b| b.into_owned()).unwrap_or_default();
    Package::parse(&read("uasset"), &read("uexp"), &read("ubulk")).map_err(|e| format!("{long}: {e}"))
}
fn export_props(pkg: &Package, index: usize) -> Result<PropList, String> {
    let body = pkg.export_bytes(index).map_err(|e| e.to_string())?;
    let mut reader = Reader::new(body);
    let ctx = props::Ctx { names: &pkg.names, file_version: pkg.summary.file_version };
    props::read_props(&mut reader, &ctx).map_err(|e| e.to_string())
}
fn named(pkg: &Package, name: &str) -> Option<PropList> {
    let i = pkg.exports.iter().position(|e| e.object_name == name)?;
    export_props(pkg, i).ok()
}

fn load_ies(set: &fn_pak::PakSet, object_path: &str) -> Result<(Vec<f32>, f32), String> {
    let pkg_path = object_path.split('.').next().unwrap_or(object_path);
    let rd = |ext: &str| set.read_game(pkg_path, ext).map(|c| c.into_owned());
    let a = rd("uasset").map_err(|e| format!("{pkg_path}: {e}"))?;
    let tex = fn_texture::Texture2D::from_files(&a, &rd("uexp").unwrap_or_default(), &rd("ubulk").unwrap_or_default()).map_err(|e| format!("{pkg_path}: {e}"))?;
    let mip = tex.best_mip().ok_or_else(|| format!("{pkg_path}: no mip"))?;
    let img = tex.decode(mip, Default::default()).map_err(|e| format!("{pkg_path}: {e}"))?;
    let row: Vec<f32> = match &img.pixels {
        fn_texture::Pixels::RgbaF16(v) => v.chunks_exact(4).take(img.width).map(|p| fn_ueio::f16_to_f32(p[0])).collect(),
        fn_texture::Pixels::Rgba8(v) => v.chunks_exact(4).take(img.width).map(|p| p[0] as f32 / 255.).collect(),
    };
    let pkg = read_package(set, pkg_path)?;
    let name = pkg_path.rsplit('/').next().unwrap_or(pkg_path);
    let mult = named(&pkg, name).and_then(|p| p.float("TextureMultiplier")).unwrap_or(1.) as f32;
    let resampled: Vec<f32> = (0..fn_render::local_lights::IES_WIDTH as usize).map(|x| { let n = row.len().max(1); row[(x * n / fn_render::local_lights::IES_WIDTH as usize).min(n - 1)] }).collect();
    Ok((if row.len() == fn_render::local_lights::IES_WIDTH as usize { row } else { resampled }, mult))
}

fn car_class(set: &fn_pak::PakSet, class: &str, cache: &mut HashMap<String, Option<CarClass>>) -> Option<CarClass> {
    if let Some(c) = cache.get(class) { return c.clone(); }
    let mut chain = vec![class.to_owned()];
    if class != CAR_PARENT { chain.insert(0, CAR_PARENT.to_owned()); }
    let mut c = CarClass::default();
    let mut ok = false;
    for k in &chain {
        let path = format!("/Game/Building/ActorBlueprints/Prop/{k}");
        let Ok(pkg) = read_package(set, &path) else { continue };
        let Some(p) = named(&pkg, &format!("Default__{k}_C")) else { continue };
        ok = true;
        if let Some(v) = p.bool("Randomly start with some cars lights on") { c.randomly_on = v; }
        if let Some(v) = p.bool("TurnHeadLightsOn") { c.head_lights_on = v; }
        if let Some(v) = p.float("HeadLightLightBrightness") { c.head = v as f32; }
        if let Some(v) = p.float("TailLightBrightness") { c.tail = v as f32; }
        if let Some(v) = p.float("Percentage chance of lights being left off") { c.chance_off = v as f32; }
    }
    let r = ok.then_some(c);
    cache.insert(class.to_owned(), r.clone());
    r
}

fn car_templates(set: &fn_pak::PakSet) -> HashMap<String, LightProps> {
    let path = format!("/Game/Building/ActorBlueprints/Prop/{CAR_PARENT}");
    let Ok(pkg) = read_package(set, &path) else { return HashMap::new() };
    pkg.exports.iter().enumerate().filter(|(_, e)| e.object_name.starts_with("NODE_AddSpotLightComponent-")).filter_map(|(i, e)| Some((e.object_name.clone(), LightProps::from_props(&pkg, &export_props(&pkg, i).ok()?)))).collect()
}

fn car_variance(actor_ue: [f32; 3]) -> f32 {
    let h = (actor_ue[0].to_bits() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (actor_ue[1].to_bits() as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ (actor_ue[2].to_bits() as u64);
    let u = ((h >> 11) as f64 / (1u64 << 53) as f64) as f32;
    0.8 + 0.2 * u
}

fn build(poi: &crate::poi::PoiSet, set: Option<&fn_pak::PakSet>) -> State {
    let mut notes = vec![];
    let effects = effects_quality();
    let dm = detail_mode(effects);
    let mut ies_rows: Vec<Vec<f32>> = vec![];
    let mut ies_index: HashMap<String, Option<IesProfile>> = HashMap::new();
    let mut cars: HashMap<String, Option<CarClass>> = HashMap::new();
    let templates = set.map(car_templates).unwrap_or_default();
    let mut entries = vec![];
    let (mut skipped_hidden, mut skipped_detail, mut skipped_quality, mut cars_on, mut cars_off, mut no_template) = (0, 0, 0, 0, 0, 0);
    for l in &poi.lights {
        let spot = l.kind == "SpotLightComponent";
        if l.kind != "PointLightComponent" && !spot { continue; }
        let inst = LightProps::from_json(&l.props);
        let is_car_node = l.component.starts_with("NODE_AddSpotLightComponent-");
        let props = if is_car_node {
            let key = l.component.rsplit_once('_').map_or(l.component.as_str(), |(a, _)| a);
            match templates.get(key) { Some(t) => t.overlay(&inst), None => { no_template += 1; continue; } }
        } else { inst };
        if l.hidden || props.visible == Some(false) || props.hidden_in_game == Some(true) || props.affects_world == Some(false) { skipped_hidden += 1; continue; }
        if props.detail_mode.unwrap_or(0) > dm { skipped_detail += 1; }
        let mut rule = Rule::Always;
        let mut intensity = None;
        if l.actor_class.starts_with("Tiered_Chest") && l.component == "TrasureLight" {
            if effects <= 1 { skipped_quality += 1; continue; }
            rule = Rule::Chest;
        }
        if is_car_node {
            let Some(c) = set.and_then(|s| car_class(s, &l.actor_class, &mut cars)) else { no_template += 1; continue };
            let on = if c.randomly_on { car_lights_on(l.actor_ue, c.chance_off) } else { c.head_lights_on };
            if !on { cars_off += 1; continue; }
            cars_on += 1;
            let head = l.component.starts_with("NODE_AddSpotLightComponent-1_") || l.component.starts_with("NODE_AddSpotLightComponent-2_");
            intensity = Some(if head { c.head } else { c.tail } * car_variance(l.actor_ue));
        }
        let ies = match (&props.ies, set) {
            (Some(path), Some(s)) => {
                let e = ies_index.entry(path.clone()).or_insert_with(|| match load_ies(s, path) {
                    Ok((row, multiplier)) => { notes.push(format!("IES {path}: TextureMultiplier {multiplier}, peak {:.3}, first {:.3}, last {:.3}", row.iter().cloned().fold(0f32, f32::max), row[0], row[row.len() - 1])); ies_rows.push(row); Some(IesProfile { row: ies_rows.len() as u32 - 1, multiplier }) }
                    Err(e) => { notes.push(format!("IES {path}: {e}")); None }
                });
                e.as_ref().map(|p| IesProfile { row: p.row, multiplier: p.multiplier })
            }
            _ => None,
        };
        if props.use_temperature == Some(true) { notes.push(format!("{}.{}: bUseTemperature not applied", l.actor_class, l.component)); }
        entries.push(Entry { actor: l.actor, rule, detail: props.detail_mode.unwrap_or(0), light: build_light(spot, &props, l.t, l.q, ies.as_ref(), intensity) });
    }
    notes.push(format!("{} local lights ({} hidden, {} above the current r.DetailMode {dm} (kept for a detail change), {} chest lights destroyed below High effects, car lights {cars_on} on / {cars_off} off, {no_template} without a template), {} IES profiles",
        entries.len(), skipped_hidden, skipped_detail, skipped_quality, ies_rows.len()));
    IES_ROWS.lock().map(|mut g| *g = Some(ies_rows)).ok();
    State { entries, last: vec![usize::MAX], notes, prop_of: HashMap::new(), props_len: usize::MAX }
}

fn active_indices(entries: &[Entry], dead: &HashSet<u32>, chest: &HashMap<u32, bool>, detail: u8) -> Vec<usize> {
    entries.iter().enumerate().filter(|(_, e)| e.detail <= detail && !dead.contains(&e.actor) && match e.rule { Rule::Always => true, Rule::Chest => chest.get(&e.actor).copied().unwrap_or(false) }).map(|(i, _)| i).collect()
}

static IES_ROWS: Mutex<Option<Vec<Vec<f32>>>> = Mutex::new(None);

static SSS_PROFILES_SENT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn sync_subsurface_profiles(renderer: &mut fn_render::Renderer) {
    let profiles = fn_pose::br_material::subsurface_profiles();
    if SSS_PROFILES_SENT.swap(profiles.len(), Ordering::Relaxed) == profiles.len() { return; }
    for (i, (path, p)) in profiles.iter().enumerate() { eprintln!("[subsurface] profile {i}: {} ScatterRadius {} SubsurfaceColor {:?} FalloffColor {:?}", if path.is_empty() { "(default)" } else { path }, p.scatter_radius, p.subsurface_color, p.falloff_color); }
    let list: Vec<fn_render::subsurface::SssProfile> = profiles.iter().map(|(_, p)| fn_render::subsurface::SssProfile { scatter_radius: p.scatter_radius, subsurface_color: p.subsurface_color, falloff_color: p.falloff_color }).collect();
    renderer.set_subsurface_profiles(&list);
}

static DYNAMIC: Mutex<(Vec<LocalLight>, bool)> = Mutex::new((Vec::new(), false));
pub fn set_dynamic(lights: Vec<LocalLight>) { if let Ok(mut d) = DYNAMIC.lock() { d.0 = lights; } }
pub fn add_dynamic(lights: impl IntoIterator<Item = LocalLight>) { if let Ok(mut d) = DYNAMIC.lock() { d.0.extend(lights); } }
fn take_dynamic() -> (Vec<LocalLight>, bool) {
    let Ok(mut d) = DYNAMIC.lock() else { return (vec![], false) };
    let had = d.1; d.1 = !d.0.is_empty();
    let l = std::mem::take(&mut d.0);
    let changed = had || !l.is_empty();
    (l, changed)
}

pub fn frame(renderer: &mut fn_render::Renderer, install: Option<PathBuf>, props: &[fn_combat::Harvestable]) {
    sync_subsurface_profiles(renderer);
    let (dynamic, dyn_changed) = take_dynamic();
    let Some(poi) = crate::poi::current() else { if dyn_changed { renderer.set_local_lights(dynamic); } return };
    let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if g.is_none() {
        let set = install.and_then(|d| fn_pak::PakSet::open_install(&d).map_err(|e| eprintln!("[local-lights] paks: {e}")).ok());
        let st = build(&poi, set.as_ref());
        for n in &st.notes { eprintln!("[local-lights] {n}"); }
        if let Some(rows) = IES_ROWS.lock().ok().and_then(|mut r| r.take()) { if !rows.is_empty() { renderer.set_ies_profiles(&rows); } }
        *g = Some(st);
    }
    let st = g.as_mut().unwrap();
    if st.props_len != props.len() {
        let owners: HashSet<u32> = st.entries.iter().map(|e| e.actor).collect();
        st.prop_of = props.iter().enumerate().filter(|(_, p)| fn_combat::is_poi_proto(p.proto) && owners.contains(&(p.proto & !fn_combat::POI_PROTO))).map(|(i, p)| (p.proto & !fn_combat::POI_PROTO, i)).collect();
        st.props_len = props.len();
    }
    let dead: HashSet<u32> = st.prop_of.iter().filter(|(_, &i)| props.get(i).is_some_and(|p| !p.alive())).map(|(&a, _)| a).collect();
    let chest: HashMap<u32, bool> = crate::chest_hook::trasure_light_states().into_iter().collect();
    let active = active_indices(&st.entries, &dead, &chest, detail_mode(effects_quality()));
    if active != st.last || dyn_changed {
        renderer.set_local_lights(active.iter().map(|&i| st.entries[i].light.clone()).chain(dynamic).collect());
        st.last = active;
    }
}

pub fn pending() -> bool { STATE.lock().map_or(true, |g| g.is_none()) }

pub fn report() -> String {
    STATE.lock().ok().and_then(|g| g.as_ref().map(|s| format!("{}; active {}", s.notes.join("; "), s.last.len()))).unwrap_or_else(|| "local lights not built".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn car_hash_is_the_blueprint_function() {
        let h = car_location_hash([0., 0., 0.]);
        let r = 5.59614f32;
        let fx = (5.25f32 / r).ceil() / r;
        let fy = fx;
        let s1 = fx + fy;
        let s2 = fy + fx * 0.04523;
        let s3 = fx + fy * -0.21458;
        let sin = |x: f32, p: f32| (x * 360. / p).to_radians().sin();
        let a = sin(s3, 0.20184);
        let b = sin(a * 0.023 + s1, 0.14587);
        let c = sin(s2 + b * 0.01914, 0.1);
        let x = c * b * 13.11458;
        assert!((h - (x - x.trunc()).abs()).abs() < 1e-6);
        assert!((0. ..1.).contains(&h));
    }
    #[test] fn searched_or_destroyed_chests_drop_their_light() {
        let p = LightProps::from_json(&serde_json::json!({"Intensity": 1700.0, "AttenuationRadius": 300.0}));
        let l = build_light(false, &p, Vec3::ZERO, Quat::IDENTITY, None, None);
        let entries = vec![Entry { actor: 7, rule: Rule::Chest, detail: 1, light: l.clone() }, Entry { actor: 9, rule: Rule::Always, detail: 1, light: l.clone() }, Entry { actor: 11, rule: Rule::Chest, detail: 1, light: l }];
        let none = HashSet::new();
        let states: HashMap<u32, bool> = [(7, true), (11, true)].into_iter().collect();
        assert_eq!(active_indices(&entries, &none, &states, 2), vec![0, 1, 2]);
        let states: HashMap<u32, bool> = [(7, false), (11, true)].into_iter().collect();
        assert_eq!(active_indices(&entries, &none, &states, 2), vec![1, 2]);
        let dead: HashSet<u32> = [9, 11].into_iter().collect();
        assert_eq!(active_indices(&entries, &dead, &states, 2), Vec::<usize>::new());
        assert_eq!(active_indices(&entries, &none, &HashMap::new(), 2), vec![1]);
        assert_eq!(active_indices(&entries, &none, &states, 0), Vec::<usize>::new());
    }
    #[test] fn detail_modes_follow_effects_quality() {
        assert_eq!((0..4).map(detail_mode).collect::<Vec<_>>(), vec![0, 1, 1, 2]);
    }
    #[test] fn street_light_props_build_the_component() {
        let j: Value = serde_json::json!({"AttenuationRadius": 1188.2867, "DetailMode": "DM_Medium", "InnerConeAngle": 2.449541, "Intensity": 80.0, "LightColor": {"a": 255, "b": 40, "g": 144, "r": 169}, "LightFalloffExponent": 4.0, "MaxDistanceFadeRange": 2048.0, "MaxDrawDistance": 20550.238, "OuterConeAngle": 48.110115, "SourceRadius": 13.316073, "VolumetricScatteringIntensity": 0.0, "bUseInverseSquaredFalloff": false});
        let p = LightProps::from_json(&j);
        assert_eq!(p.detail_mode, Some(1));
        let q = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let l = build_light(true, &p, Vec3::new(100., 200., 300.), q, None, None);
        assert_eq!(l.position_m, Vec3::new(1., 3., 2.));
        assert!(!l.inverse_squared);
        assert_eq!(l.falloff_exponent, 4.);
        assert!((l.color[0] - srgb_to_linear(169) * 80.).abs() < 1e-4);
        assert_eq!(l.spot_cone_deg, Some((2.449541, 48.110115)));
        assert!((l.forward - Vec3::new(0., -1., 0.)).length() < 1e-5);
        assert!(l.cast_shadows);
    }
    #[test] fn ies_multiplier_scales_the_brightness() {
        let p = LightProps::from_json(&serde_json::json!({"Intensity": 2500.0, "LightColor": {"r": 255, "g": 255, "b": 255, "a": 255}}));
        let l = build_light(false, &p, Vec3::ZERO, Quat::IDENTITY, Some(&IesProfile { row: 3, multiplier: 4.287164 }), None);
        assert!((l.color[0] - 2500. * 4.287164).abs() < 1e-2);
        assert_eq!(l.ies_row, Some(3));
        assert!(l.inverse_squared);
        assert_eq!(l.attenuation_radius_cm, 1000.);
    }
}
