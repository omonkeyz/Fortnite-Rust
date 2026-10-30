use crate::data::{cdo_chain, curve_row, pkg_of};
use fn_pose::{loader, ClipSourceDir};
use fn_ueio::props::{read_props, Ctx, PropList, PropValue};
use fn_ueio::{Package, Reader};
use glam::Vec3;

pub const DEFAULT_SPEED_SCALE_RANGE: (f32, f32) = (2048., 5000.);
pub const DEFAULT_SPEED_SCALE_MULT: (f32, f32) = (1., 1.5);
pub const DEFAULT_MESH_SCALE_MULT: [f32; 3] = [1., 1., 1.];
pub const DEFAULT_MESH_SCALE_TIME: f32 = 10.;
pub const SCALE_MIN: [f32; 3] = [0., 1., 1.];
pub const SCALE_MAX: [f32; 3] = [1., 1., 1.];
pub const DEAD_SPEED: f32 = 10.;
pub const MIN_AXIS_SCALE: f32 = 0.0001;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TracerDef {
    pub class: String,
    pub mesh: String,
    pub override_materials: Vec<Option<String>>,
    pub mesh_min_x: f32,
    pub mesh_max_x: f32,
    pub mesh_scale_mult: [f32; 3],
    pub mesh_scale_time: f32,
    pub speed: f32,
    pub speed_row: String,
    pub scale_speed: bool,
    pub speed_scale_range: (f32, f32),
    pub speed_scale_mult: (f32, f32),
    pub scale_on_death: bool,
    pub pass_by_radii: (f32, f32),
    pub pass_by_far_sound: Option<String>,
    pub pass_by_close_sound: Option<String>,
}

fn props_at(pkg: &Package, i: usize) -> Option<PropList> {
    let (s, e) = pkg.export_range(i);
    read_props(&mut Reader::at(&pkg.data[..e], s), &Ctx { names: &pkg.names, file_version: pkg.summary.file_version }).ok()
}

fn obj_path(pkg: &Package, v: &PropValue) -> Option<String> {
    match v {
        PropValue::Object(i) if *i < 0 => Some(pkg.import_path(*i)),
        PropValue::Str(s) if s.starts_with("/Game/") => Some(s.clone()),
        _ => None,
    }
}

fn vec3(v: &PropValue) -> Option<[f32; 3]> {
    match v {
        PropValue::Vec3(x) => Some(*x),
        PropValue::Struct { fields, .. } => Some([fields.float("X")? as f32, fields.float("Y")? as f32, fields.float("Z")? as f32]),
        _ => None,
    }
}

fn bounds_x(src: &ClipSourceDir, mesh: &str) -> Result<(f32, f32), String> {
    let pkg = loader::package(src, pkg_of(mesh))?;
    let i = (0..pkg.exports.len()).find(|&i| pkg.export_class(i) == "StaticMesh").ok_or_else(|| format!("{mesh}: no StaticMesh export"))?;
    let p = props_at(&pkg, i).ok_or_else(|| format!("{mesh}: unreadable"))?;
    let b = p.get("ExtendedBounds").ok_or_else(|| format!("{mesh}: no ExtendedBounds"))?;
    let (o, e) = match b {
        PropValue::Struct { fields, .. } => (fields.get("Origin").and_then(vec3), fields.get("BoxExtent").and_then(vec3)),
        PropValue::Raw(r) if r.len() >= 24 => {
            let f = |k: usize| f32::from_le_bytes(r[k * 4..k * 4 + 4].try_into().unwrap());
            (Some([f(0), f(1), f(2)]), Some([f(3), f(4), f(5)]))
        }
        _ => (None, None),
    };
    let (o, e) = o.zip(e).ok_or_else(|| format!("{mesh}: ExtendedBounds not decoded"))?;
    Ok((o[0] - e[0], o[0] + e[0]))
}

pub fn load(src: &ClipSourceDir, class: &str) -> Result<TracerDef, String> {
    let chain = cdo_chain(src, class)?;
    let f = |k: &str| chain.iter().find_map(|(_, p)| p.float(k)).map(|v| v as f32);
    let b = |k: &str| chain.iter().find_map(|(_, p)| p.bool(k));
    let v = |k: &str| chain.iter().find_map(|(_, p)| p.get(k).and_then(vec3));
    let path = |k: &str| chain.iter().find_map(|(pk, p)| p.get(k).and_then(|x| obj_path(pk, x)));
    let mut mesh = None;
    let mut overrides: Vec<Option<String>> = vec![];
    for (pkg, _) in &chain {
        for i in 0..pkg.exports.len() {
            if pkg.exports[i].object_name != "TracerMesh0" || pkg.export_class(i) != "StaticMeshComponent" { continue; }
            let Some(p) = props_at(pkg, i) else { continue };
            if mesh.is_none() { mesh = p.get("StaticMesh").and_then(|x| obj_path(pkg, x)); }
            if overrides.is_empty() { if let Some(PropValue::Array(a)) = p.get("OverrideMaterials") { overrides = a.iter().map(|x| obj_path(pkg, x)).collect(); } }
        }
    }
    let mesh = mesh.ok_or_else(|| format!("{class}: TracerMesh0 has no StaticMesh"))?;
    let (mesh_min_x, mesh_max_x) = bounds_x(src, &mesh)?;
    let (speed, speed_row) = {
        let ClipSourceDir::Pak(set) = src else { return Err("tracer speed curve is read from the pak".into()) };
        let (pk, h) = chain.iter().find_map(|(pk, p)| p.fields("Speed").map(|h| (pk, h))).ok_or_else(|| format!("{class}: no Speed handle"))?;
        let table = h.object("CurveTable").filter(|i| *i < 0).map(|i| pk.import_path(i)).ok_or("Speed handle has no CurveTable")?;
        let row = h.text("RowName").ok_or("Speed handle has no RowName")?.to_owned();
        (curve_row(set, pkg_of(&table), &row, 0.)?, row)
    };
    Ok(TracerDef {
        class: pkg_of(class).to_owned(),
        mesh,
        override_materials: overrides,
        mesh_min_x,
        mesh_max_x,
        mesh_scale_mult: v("MeshScaleMult").unwrap_or(DEFAULT_MESH_SCALE_MULT),
        mesh_scale_time: f("MeshScaleTime").unwrap_or(DEFAULT_MESH_SCALE_TIME),
        speed,
        speed_row,
        scale_speed: b("bScaleSpeed").unwrap_or(true),
        speed_scale_range: (f("SpeedScaleMinRange").unwrap_or(DEFAULT_SPEED_SCALE_RANGE.0), f("SpeedScaleMaxRange").unwrap_or(DEFAULT_SPEED_SCALE_RANGE.1)),
        speed_scale_mult: (f("SpeedScaleMinMultiplier").unwrap_or(DEFAULT_SPEED_SCALE_MULT.0), f("SpeedScaleMaxMultiplier").unwrap_or(DEFAULT_SPEED_SCALE_MULT.1)),
        scale_on_death: b("bScaleOnDeath").unwrap_or(true),
        pass_by_radii: (f("PassByRadiusMin").unwrap_or(0.), f("PassByRadiusMax").unwrap_or(0.)),
        pass_by_far_sound: path("PassByFarSound"),
        pass_by_close_sound: path("PassByCloseSound"),
    })
}

pub fn component_particles(src: &ClipSourceDir, class: &str) -> Vec<(String, [f32; 3])> {
    let Ok(chain) = cdo_chain(src, class) else { return vec![] };
    let mut out: Vec<(String, String, [f32; 3])> = vec![];
    for (pkg, _) in &chain {
        for i in 0..pkg.exports.len() {
            if pkg.export_class(i) != "ParticleSystemComponent" { continue; }
            let name = pkg.exports[i].object_name.clone();
            if out.iter().any(|o| o.0 == name) { continue; }
            let Some(p) = props_at(pkg, i) else { continue };
            let Some(t) = p.get("Template").and_then(|x| obj_path(pkg, x)) else { continue };
            out.push((name, t, p.get("RelativeLocation").and_then(vec3).unwrap_or([0.; 3])));
        }
    }
    out.into_iter().map(|(_, t, l)| (t, l)).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tracer {
    pub pos: Vec3,
    pub vel: Vec3,
    pub end: Vec3,
    pub start: Vec3,
    pub scale: f32,
    pub scale_time: f32,
    pub dead: bool,
    pub scaled_up: bool,
    pub scale_on_death: bool,
    pub mult: [f32; 3],
}

impl Tracer {
    pub fn new(def: &TracerDef, start: Vec3, end: Vec3) -> Tracer {
        let mut speed = def.speed;
        if def.scale_speed {
            let span = def.speed_scale_range.1 - def.speed_scale_range.0;
            let mut a = 0.;
            if span > 0. { a = ((end - start).length() - def.speed_scale_range.0) / span; if !(a >= 0.) { a = 0.; } else if a >= 1. { a = 1.; } }
            speed *= (def.speed_scale_mult.1 - def.speed_scale_mult.0) * a + def.speed_scale_mult.0;
        }
        let d = end - start;
        let dir = if d.length_squared() > 1e-8 { d.normalize() } else { d };
        let mut scale_time = def.mesh_scale_time;
        let len = (def.mesh_max_x - def.mesh_min_x) * def.mesh_scale_mult[0];
        if speed > 0. && len > 0. { let t = 1. / (speed / len); if scale_time <= t && t != scale_time { scale_time = t; } }
        Tracer { pos: start, vel: dir * speed, end, start, scale: 0., scale_time, dead: false, scaled_up: false, scale_on_death: def.scale_on_death, mult: def.mesh_scale_mult }
    }

    pub fn tick(&mut self, dt: f32) -> bool {
        if !self.dead {
            let ahead = (self.end - self.pos).dot(self.vel) > 0.;
            let moving = self.vel.x.abs() > DEAD_SPEED || self.vel.y.abs() > DEAD_SPEED || self.vel.z.abs() > DEAD_SPEED;
            if !(ahead && moving) { self.dead = true; }
        }
        if !self.dead {
            if !self.scaled_up { self.scale = (dt / self.scale_time + self.scale).min(1.); if self.scale >= 1. { self.scaled_up = true; } }
        } else if !self.scale_on_death {
            self.scale = 0.;
        } else {
            self.scale = (self.scale - dt / self.scale_time).max(0.);
        }
        if self.scale <= 0. { return false; }
        if !self.dead { self.pos += self.vel * dt; }
        true
    }

    pub fn scale3(&self) -> Vec3 {
        let s = self.scale;
        let l = |k: usize| (SCALE_MAX[k] - SCALE_MIN[k]) * s + SCALE_MIN[k];
        Vec3::new((l(0) * self.mult[0]).max(MIN_AXIS_SCALE), (l(1) * self.mult[1]).max(MIN_AXIS_SCALE), (l(2) * self.mult[2]).max(MIN_AXIS_SCALE))
    }

    pub fn dir(&self) -> Vec3 { (self.end - self.start).normalize_or_zero() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn def() -> TracerDef {
        TracerDef { mesh_min_x: -162.66, mesh_max_x: 0., mesh_scale_mult: [8., 1., 1.], mesh_scale_time: 0.05, speed: 30000., scale_speed: true, speed_scale_range: DEFAULT_SPEED_SCALE_RANGE,
            speed_scale_mult: (2., 2.), scale_on_death: true, ..Default::default() }
    }
    #[test] fn speed_scales_with_distance_between_the_native_range_bounds() {
        let mut d = def(); d.speed_scale_mult = DEFAULT_SPEED_SCALE_MULT;
        let near = Tracer::new(&d, Vec3::ZERO, Vec3::X * 1000.);
        let far = Tracer::new(&d, Vec3::ZERO, Vec3::X * 9000.);
        let mid = Tracer::new(&d, Vec3::ZERO, Vec3::X * 3524.);
        assert!((near.vel.length() - 30000.).abs() < 1e-2 && (far.vel.length() - 45000.).abs() < 1e-1 && (mid.vel.length() - 37500.).abs() < 1.);
        assert!((Tracer::new(&def(), Vec3::ZERO, Vec3::X * 9000.).vel.length() - 60000.).abs() < 1e-1);
    }
    #[test] fn scale_time_covers_the_scaled_mesh_length_at_least() {
        let mut d = def(); d.speed = 10000.; d.speed_scale_mult = (1., 1.);
        let t = Tracer::new(&d, Vec3::ZERO, Vec3::X * 5000.);
        assert!((t.scale_time - 162.66 * 8. / 10000.).abs() < 1e-4);
        assert!((Tracer::new(&def(), Vec3::ZERO, Vec3::X * 5000.).scale_time - 0.05).abs() < 1e-6);
    }
    #[test] fn grows_flies_dies_past_the_end_and_shrinks_in_place() {
        let mut t = Tracer::new(&def(), Vec3::ZERO, Vec3::X * 3000.);
        let mut frames = 0;
        let mut died_at = None;
        while t.tick(1. / 60.) {
            frames += 1;
            if t.dead && died_at.is_none() { died_at = Some(t.pos); }
            assert!(frames < 200);
        }
        let p = died_at.expect("died");
        assert!(p.x >= 3000. && p.x < 3000. + 60000. / 60. + 1., "{p:?}");
        assert!(t.scale == 0.);
        let mut u = Tracer::new(&def(), Vec3::ZERO, Vec3::X * 3000.);
        u.tick(0.025);
        assert!((u.scale - 0.5).abs() < 1e-5 && (u.scale3().x - 4.).abs() < 1e-4 && u.scale3().y == 1.);
    }
    #[test] fn without_scale_on_death_it_vanishes_the_tick_it_dies() {
        let mut d = def(); d.scale_on_death = false;
        let mut t = Tracer::new(&d, Vec3::ZERO, Vec3::X * 100.);
        assert!(t.tick(1. / 60.));
        assert!(!t.tick(1. / 60.));
    }
}
