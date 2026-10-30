//! ROADS / SPLINES (2026-10-08): the cooked `SplineMeshComponent`s of 1.8 (`fn-export props splines` -> `<props>/splines/landscape_splines.json`) drawn with
//! UE4.16's spline-mesh deformation on the real cooked-material path (the `FSplineMeshVertexFactory` permutation of each material's shader map, fn_fx gpu.rs
//! `Vf::SplineMesh`), like the POI statics of `statics_render.rs` but with the per-component spline parameters in each vertex program's `$Globals`.
//!
//! WHAT THEY ARE: in 1.8 every one is a landscape-spline mesh `S_Athena_Stream_2W_Straight_1L_a` (139 components in Athena_Terrain_LS_01/02/03/05): the
//! streams / rivers (bank `Athena_Terrain_Mat_Spline` + masked water `Stream_Flowmaps_01_Reverse_Spline_Inst`). No road is a spline in 1.8: off-POI roads are
//! the `Road_2` layer of the landscape material (landscape_render.rs), town roads the POI / LS static meshes (statics_render.rs).
//!
//! UE4.16 formulas ported here (and tested below):
//! * `SplineEvalPos` / `SplineEvalDir` (cubic Hermite and its normalized derivative): SplineMeshComponent.cpp:681-698 = LocalVertexFactory.usf:396-414.
//! * `CalcSliceTransformAtSplineOffset` (frame from `SplineUpDir`, offset, roll, scale lerped by `bSmoothInterpRollScale ? SmoothStep : Alpha`):
//!   SplineMeshComponent.cpp:866-916 = the shader's `CalcSliceTransform` (LocalVertexFactory.usf:417-460).
//! * `CalculateScaleZAndMinZ` (the vertex's Alpha = dot(pos, SplineMeshDir) * ScaleZ - MinZ): SplineMeshComponent.cpp:531-544.
//! * `CalcBounds` (the deformed bounds: the slice at MinT / MaxT and the derivative's roots per axis applied to the flattened mesh box):
//!   SplineMeshComponent.cpp:701-806; it is the component's local bounds for culling and the `Primitive` buffer.
//! * `FSplineMeshVertexFactoryShaderParameters::SetMesh` (SplineMeshComponent.cpp:59-104): the 17 values, `SplineMeshDir` / `X` / `Y` = the masks of
//!   ForwardAxis, +1, +2; their slots come from the program's serialized `Serialize` order (SplineMeshSceneProxy.h:65-88: 17 `FShaderParameter`s = 102 bytes;
//!   measured on Stream_Flowmaps_01_Spline_inst: StartPos cb0 byte 208 ... SplineMeshY 368).
//! * `FSplineMeshSceneProxy` (SplineMeshSceneProxy.cpp:6-80): `bSupportsDistanceFieldRepresentation = false` (no distance-field object), ReverseCulling ^=
//!   (StartScale.X < 0) ^ (StartScale.Y < 0), a material without `MATUSAGE_SplineMesh` would draw the default material (both 1.8 materials have the usage).
//! Culling / LOD as the statics: frustum on the deformed bounds, the HZB occlusion (fn_render::occlusion), the component draw distance (0 here: none), the
//! cooked LOD screen sizes (`statics_render::static_mesh_lod`; the stream mesh has one LOD), shadows into the cascades within the CSM reach.
//! Mobility Static (USplineMeshComponent ctor), so velocity only with a position-modifying material (as statics_render).
//! NOT PORTED (named): collision of the stream meshes (BlockAll / QueryOnly: the pawn walks the landscape there); the browser build streams nothing for it yet.
//! `FN_ROADS=0` switches the whole path off (the spline meshes are not drawn).
use fn_fx::gpu::{build_material_layout, LayoutOpts, MaterialGpu, SharedBuffers, TextureCache, Vf};
use fn_fx::material::Material;
use fn_fx::view::{bytes, sprite_vf_buffer, ViewParams, VIEW_FLOAT4S};
use fn_pak::PakSet;
use fn_render::fx::*;
use fn_render::primitive_ub::BoxSphereBounds;
use glam::{Mat3, Mat4, Quat, Vec2, Vec3};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// `FN_ROADS=0`: no spline meshes.
pub fn enabled() -> bool { fn_live::var("FN_ROADS").map_or(true, |v| v != "0") }

// ---- UE4.16 spline-mesh math -------------------------------------------------------------------------------------------------------------------

/// `FSplineMeshParams` (component space, cm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplineParams { pub start_pos: Vec3, pub start_tangent: Vec3, pub start_scale: Vec2, pub start_roll: f32, pub start_offset: Vec2, pub end_pos: Vec3, pub end_tangent: Vec3, pub end_scale: Vec2, pub end_roll: f32, pub end_offset: Vec2 }
impl Default for SplineParams {
    /// The `USplineMeshComponent` constructor's values (SplineMeshComponent.cpp:278-284).
    fn default() -> Self { SplineParams { start_pos: Vec3::ZERO, start_tangent: Vec3::new(100., 0., 0.), start_scale: Vec2::ONE, start_roll: 0., start_offset: Vec2::ZERO, end_pos: Vec3::new(100., 0., 0.), end_tangent: Vec3::new(100., 0., 0.), end_scale: Vec2::ONE, end_roll: 0., end_offset: Vec2::ZERO } }
}

/// What `FSplineMeshSceneProxy` copies from the component.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplineMesh { pub p: SplineParams, pub up_dir: Vec3, /** ESplineMeshAxis: 0 X, 1 Y, 2 Z */ pub forward_axis: usize, pub smooth_interp_roll_scale: bool, pub boundary_min: f32, pub boundary_max: f32 }
impl Default for SplineMesh { fn default() -> Self { SplineMesh { p: SplineParams::default(), up_dir: Vec3::Z, forward_axis: 0, smooth_interp_roll_scale: false, boundary_min: 0., boundary_max: 0. } } }

/// `SplineEvalPos` (SplineMeshComponent.cpp:681).
pub fn eval_pos(sp: Vec3, st: Vec3, ep: Vec3, et: Vec3, a: f32) -> Vec3 {
    let (a2, a3) = (a * a, a * a * a);
    ((2. * a3) - (3. * a2) + 1.) * sp + (a3 - (2. * a2) + a) * st + (a3 - a2) * et + ((-2. * a3) + (3. * a2)) * ep
}
/// `SplineEvalDir` (SplineMeshComponent.cpp:689): the normalized derivative (`GetSafeNormal`: zero for a degenerate vector).
pub fn eval_dir(sp: Vec3, st: Vec3, ep: Vec3, et: Vec3, a: f32) -> Vec3 {
    let c = 6. * sp + 3. * st + 3. * et - 6. * ep;
    let d = -6. * sp - 4. * st - 2. * et + 6. * ep;
    safe_normal(c * a * a + d * a + st)
}
/// `FVector::GetSafeNormal` (tolerance SMALL_NUMBER 1e-8 on the squared size).
fn safe_normal(v: Vec3) -> Vec3 { let s = v.length_squared(); if s == 1. { v } else if s < 1e-8 { Vec3::ZERO } else { v / s.sqrt() } }
/// `SmoothStep` (SplineMeshComponent.cpp:667).
pub fn smooth_step(a: f32, b: f32, x: f32) -> f32 { if x < a { 0. } else if x >= b { 1. } else { let f = (x - a) / (b - a); f * f * (3. - 2. * f) } }
fn axis_value(v: Vec3, axis: usize) -> f32 { v[axis.min(2)] }
fn axis_mask(axis: usize) -> Vec3 { let mut m = Vec3::ONE; m[axis.min(2)] = 0.; m }

impl SplineMesh {
    /// `CalculateScaleZAndMinZ` with the mesh's `GetBounds()` (= ExtendedBounds): (SplineMeshScaleZ, SplineMeshMinZ).
    pub fn scale_z_min_z(&self, mesh: &BoxSphereBounds) -> (f32, f32) {
        if (self.boundary_min - self.boundary_max).abs() <= 1e-8 {
            let s = 0.5 / axis_value(Vec3::from(mesh.extent), self.forward_axis);
            (s, axis_value(Vec3::from(mesh.origin), self.forward_axis) * s - 0.5)
        } else {
            let s = 1. / (self.boundary_max - self.boundary_min);
            (s, self.boundary_min * s)
        }
    }
    /// `CalcSliceTransformAtSplineOffset(Alpha)` as a matrix (column-vector glam: R * S then T; `FTransform(SplineDir, XVec, YVec, Pos)` rows -> columns).
    pub fn slice_transform(&self, alpha: f32) -> Mat4 {
        let p = &self.p;
        let h = if self.smooth_interp_roll_scale { smooth_step(0., 1., alpha) } else { alpha };
        let mut pos = eval_pos(p.start_pos, p.start_tangent, p.end_pos, p.end_tangent, alpha);
        let dir = eval_dir(p.start_pos, p.start_tangent, p.end_pos, p.end_tangent, alpha);
        let bx = safe_normal(self.up_dir.cross(dir));
        let by = safe_normal(dir.cross(bx));
        let off = p.start_offset.lerp(p.end_offset, h);
        pos += off.x * bx + off.y * by;
        let roll = p.start_roll + (p.end_roll - p.start_roll) * h;
        let (s, c) = roll.sin_cos();
        let xv = c * bx - s * by;
        let yv = c * by + s * bx;
        let sc = p.start_scale.lerp(p.end_scale, h);
        let (rot, scale) = match self.forward_axis {
            0 => (Mat3::from_cols(dir, xv, yv), Vec3::new(1., sc.x, sc.y)),
            1 => (Mat3::from_cols(yv, dir, xv), Vec3::new(sc.y, 1., sc.x)),
            _ => (Mat3::from_cols(xv, yv, dir), Vec3::new(sc.x, sc.y, 1.)),
        };
        Mat4::from_cols(rot.x_axis.extend(0.) * scale.x, rot.y_axis.extend(0.) * scale.y, rot.z_axis.extend(0.) * scale.z, pos.extend(1.))
    }
    /// The vertex program's deformation of a mesh-local vertex (LocalVertexFactory.usf `CalcWorldPosition` before `LocalToWorld`): the forward coordinate picks
    /// the slice (`Alpha = ZPos * ScaleZ - MinZ`), the other two are placed in it (XVec * scale.x, YVec * scale.y), the forward one dropped.
    pub fn deform(&self, v: Vec3, mesh: &BoxSphereBounds) -> Vec3 {
        let (sz, mz) = self.scale_z_min_z(mesh);
        let alpha = axis_value(v, self.forward_axis) * sz - mz;
        self.slice_transform(alpha).transform_point3(v * axis_mask(self.forward_axis))
    }
    /// `CalcBounds(Identity)`: the component-space bounds of the deformed mesh.
    pub fn local_bounds(&self, mesh: &BoxSphereBounds) -> BoxSphereBounds {
        let (mut min_t, mut max_t) = (0f32, 1f32);
        let (o, e) = (Vec3::from(mesh.origin), Vec3::from(mesh.extent));
        if (self.boundary_min - self.boundary_max).abs() > 1e-8 {
            let (mn, mx) = (axis_value(o - e, self.forward_axis), axis_value(o + e, self.forward_axis));
            let (a, b) = ((mn - self.boundary_min) / (self.boundary_max - self.boundary_min), (mx - self.boundary_min) / (self.boundary_max - self.boundary_min));
            if a.abs() < 4. && b.abs() < 4. { min_t = a; max_t = b; } // MaxSplineExtrapolation
        }
        let mask = axis_mask(self.forward_axis);
        let (fo, fe) = (o * mask, e * mask);
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        let mut add = |t: f32| {
            let m = self.slice_transform(t);
            for k in 0..8 { let c = fo + Vec3::new(if k & 1 == 0 { -fe.x } else { fe.x }, if k & 2 == 0 { -fe.y } else { fe.y }, if k & 4 == 0 { -fe.z } else { fe.z }); let w = m.transform_point3(c); lo = lo.min(w); hi = hi.max(w); }
        };
        add(min_t); add(max_t);
        let p = &self.p;
        let a = 6. * p.start_pos + 3. * p.start_tangent + 3. * p.end_tangent - 6. * p.end_pos;
        let b = -6. * p.start_pos - 4. * p.start_tangent - 2. * p.end_tangent + 6. * p.end_pos;
        let c = p.start_tangent;
        let disc = b * b - 4. * a * c;
        for i in 0..3 {
            if disc[i] > 0. && a[i].abs() > 1e-8 {
                let (sq, den) = (disc[i].sqrt(), 0.5 / a[i]);
                for t in [(-b[i] + sq) * den, (-b[i] - sq) * den] { if t >= min_t && t <= max_t { add(t); } }
            }
        }
        let (center, ext) = ((lo + hi) * 0.5, (hi - lo) * 0.5);
        BoxSphereBounds { origin: center.to_array(), extent: ext.to_array(), radius: ext.length() }
    }
    /// The 17 `SetMesh` values in `Serialize` order (SplineMeshSceneProxy.h:67-87), each as the floats `SetShaderValue` writes (the bool as a u32).
    pub fn shader_values(&self, mesh: &BoxSphereBounds) -> [Vec<f32>; 17] {
        let p = &self.p;
        let (sz, mz) = self.scale_z_min_z(mesh);
        let mask = |k: usize| { let mut m = [0f32; 3]; m[k % 3] = 1.; m.to_vec() };
        [p.start_pos.to_array().to_vec(), p.start_tangent.to_array().to_vec(), vec![p.start_roll], p.start_scale.to_array().to_vec(), p.start_offset.to_array().to_vec(),
         p.end_pos.to_array().to_vec(), p.end_tangent.to_array().to_vec(), vec![p.end_roll], p.end_scale.to_array().to_vec(), p.end_offset.to_array().to_vec(),
         self.up_dir.to_array().to_vec(), vec![f32::from_bits(self.smooth_interp_roll_scale as u32)], vec![mz], vec![sz],
         mask(self.forward_axis), mask(self.forward_axis + 1), mask(self.forward_axis + 2)]
    }
    /// `ReverseCulling ^= (StartScale.X < 0) ^ (StartScale.Y < 0)` (SplineMeshSceneProxy.cpp:55).
    pub fn flips_culling(&self) -> bool { (self.p.start_scale.x < 0.) ^ (self.p.start_scale.y < 0.) }
}

/// The spline VF's 17 serialized `FShaderParameter`s of a program compiled for `FSplineMeshVertexFactory` (`None` for another VF / layout).
pub const SPLINE_VF_PARAM_BYTES: usize = 17 * 6;
fn spline_vf_slots(p: &fn_fx::shader_params::MaterialShaderParams) -> Option<[fn_fx::shader_params::ShaderParam; 17]> {
    if p.vertex_factory.as_deref() != Some("FSplineMeshVertexFactory") || p.vf_params.len() != SPLINE_VF_PARAM_BYTES { return None; }
    let b = &p.vf_params;
    Some(std::array::from_fn(|k| fn_fx::shader_params::ShaderParam { base_index: u16::from_le_bytes([b[6 * k], b[6 * k + 1]]), num_bytes: u16::from_le_bytes([b[6 * k + 2], b[6 * k + 3]]), buffer_index: u16::from_le_bytes([b[6 * k + 4], b[6 * k + 5]]) }))
}
/// `SetShaderValue` of each bound spline value into a vertex globals buffer (`floats` = its float4s; `slot` = its constant buffer).
fn write_spline(floats: &mut [f32], g: &fn_fx::gpu::StageGlobals, vals: &[Vec<f32>; 17]) -> bool {
    let Some(slots) = spline_vf_slots(&g.params) else { return false };
    for (sp, v) in slots.iter().zip(vals) {
        if !sp.is_bound() || sp.buffer_index != g.slot { continue; }
        for (k, f) in v.iter().enumerate().take(sp.num_bytes as usize / 4) { if let Some(x) = floats.get_mut(sp.float_index() + k) { *x = *f; } }
    }
    true
}
fn to_floats(b: &[u8]) -> Vec<f32> { b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect() }
fn to_bytes(f: &[f32]) -> Vec<u8> { fn_fx::view::f32_bytes(f) }

// ---- the export file ---------------------------------------------------------------------------------------------------------------------------

/// One component of `landscape_splines.json`.
#[derive(Clone, Debug)]
pub struct Component { pub level: String, pub name: String, pub mesh: String, pub overrides: Vec<Option<String>>, pub t: Vec3, pub q: Quat, pub s: Vec3, pub spline: SplineMesh, pub cast_shadow: bool, pub cull_cm: f32 }
fn f3(v: &Value, d: Vec3) -> Vec3 { v.as_array().filter(|a| a.len() >= 3).map(|a| Vec3::new(a[0].as_f64().unwrap_or(d.x as f64) as f32, a[1].as_f64().unwrap_or(d.y as f64) as f32, a[2].as_f64().unwrap_or(d.z as f64) as f32)).unwrap_or(d) }
fn f2(v: &Value, d: Vec2) -> Vec2 { v.as_array().filter(|a| a.len() >= 2).map(|a| Vec2::new(a[0].as_f64().unwrap_or(d.x as f64) as f32, a[1].as_f64().unwrap_or(d.y as f64) as f32)).unwrap_or(d) }
pub fn parse_components(doc: &Value) -> Vec<Component> {
    let d = SplineParams::default();
    doc["components"].as_array().map(|a| a.as_slice()).unwrap_or(&[]).iter().filter_map(|c| {
        let p = &c["params"];
        let q = c["q"].as_array().filter(|a| a.len() >= 4).and_then(|a| Some(Quat::from_xyzw(a[0].as_f64()? as f32, a[1].as_f64()? as f32, a[2].as_f64()? as f32, a[3].as_f64()? as f32))).unwrap_or(Quat::IDENTITY);
        Some(Component {
            level: c["level"].as_str().unwrap_or("").to_owned(), name: c["name"].as_str().unwrap_or("").to_owned(), mesh: c["mesh"].as_str()?.to_owned(),
            overrides: c["overrides"].as_array().map(|a| a.iter().map(|v| v.as_str().filter(|s| !s.is_empty()).map(String::from)).collect()).unwrap_or_default(),
            t: f3(&c["t"], Vec3::ZERO), q, s: f3(&c["s"], Vec3::ONE),
            spline: SplineMesh {
                p: SplineParams { start_pos: f3(&p["start_pos"], d.start_pos), start_tangent: f3(&p["start_tangent"], d.start_tangent), start_scale: f2(&p["start_scale"], d.start_scale), start_roll: p["start_roll"].as_f64().unwrap_or(0.) as f32, start_offset: f2(&p["start_offset"], d.start_offset),
                    end_pos: f3(&p["end_pos"], d.end_pos), end_tangent: f3(&p["end_tangent"], d.end_tangent), end_scale: f2(&p["end_scale"], d.end_scale), end_roll: p["end_roll"].as_f64().unwrap_or(0.) as f32, end_offset: f2(&p["end_offset"], d.end_offset) },
                up_dir: f3(&c["up_dir"], Vec3::Z), forward_axis: c["forward_axis"].as_u64().unwrap_or(0).min(2) as usize, smooth_interp_roll_scale: c["smooth_interp_roll_scale"].as_bool().unwrap_or(false),
                boundary_min: c["boundary_min"].as_f64().unwrap_or(0.) as f32, boundary_max: c["boundary_max"].as_f64().unwrap_or(0.) as f32,
            },
            cast_shadow: c["cast_shadow"].as_bool().unwrap_or(true), cull_cm: c["cull_cm"].as_f64().unwrap_or(0.) as f32,
        })
    }).collect()
}

// ---- the renderer ------------------------------------------------------------------------------------------------------------------------------

pub const VS_TYPE: &str = "TBasePassVSFNoLightMapPolicy";
pub const PS_TYPES: [&str; 2] = ["TBasePassPSFNoLightMapPolicy", "TBasePassPSFNoLightMapPolicySkylight"];
/// The statics' CSM reach (statics_render.rs `POI_CSM_CM`: GetCSMMaxDistance 4500 x r.Shadow.DistanceScale 2 + 50 m caster extrusion).
const CSM_CM: f32 = 9000. + 5000.;

struct Entry { mat: Arc<Material>, gpu: MaterialGpu, velocity: bool }
struct Lod { mesh: FxMeshId, num_uv: usize, sections: Vec<(usize, u32, u32)> }
struct MeshG { lods: Vec<Lod>, mats: Vec<Option<String>>, screen_size: [f32; 8], bounds: BoxSphereBounds }
struct Placed {
    c: Component, mesh: usize, l2w: Mat4, local: BoxSphereBounds, values: [Vec<f32>; 17], reverse: bool,
    /// per LOD section: (base-pass entry, shadow entry)
    secs: Vec<Vec<(Option<usize>, Option<usize>)>>,
    prim: Option<FxBufId>, vs: HashMap<(usize, bool), FxBufId>, binds: HashMap<(usize, bool), Vec<(u32, FxBindId)>>,
}

pub struct SplineRender {
    set: Arc<PakSet>, file: PathBuf, cache: TextureCache, shared: Option<SharedBuffers>, fade: Option<FxBufId>,
    meshes: Vec<MeshG>, mesh_of: HashMap<String, Option<usize>>,
    entries: Vec<Entry>, entry_of: HashMap<(String, usize, bool), Option<usize>>,
    shadows: Vec<Option<MaterialGpu>>, // per entry
    placed: Vec<Placed>, built: bool, log: Vec<String>, pending: Option<(std::collections::VecDeque<Component>, Mat4, f32)>,
    pub stats: Stats,
}
#[derive(Clone, Debug, Default)]
pub struct Stats { pub components: usize, pub drawn: usize, pub culled_frustum: usize, pub culled_distance: usize, pub culled_occlusion: usize, pub draws: usize, pub shadow_draws: usize }

impl SplineRender {
    pub fn new(install: &Path, props_dir: &Path) -> Result<SplineRender, String> {
        let file = props_dir.join("splines").join("landscape_splines.json");
        if !file.is_file() { return Err(format!("{} missing (fn-export props splines)", file.display())); }
        let set = Arc::new(PakSet::open_install(install).map_err(|e| e.to_string())?);
        Ok(SplineRender { set, file, cache: TextureCache::default(), shared: None, fade: None, meshes: vec![], mesh_of: HashMap::new(), entries: vec![], entry_of: HashMap::new(), shadows: vec![], placed: vec![], built: false, pending: None, log: vec![], stats: Stats::default() })
    }
    fn note(&mut self, s: String) { eprintln!("[splines] {s}"); self.log.push(s); }

    fn init_gpu(&mut self, fx: &mut FxRenderer) {
        if self.shared.is_some() { return; }
        let view = fx.create_buffer("splines view", VIEW_FLOAT4S * 16);
        let light = fx.create_buffer("splines light", fn_render::sky_light::FX_LIGHT_BYTES);
        let sprite_vf = fx.create_buffer("splines sprite vf", 10 * 16);
        fx.write_buffer(sprite_vf, &bytes(&sprite_vf_buffer()));
        let prim = fx.create_buffer("splines primitive (shared)", 32 * 16);
        let fade = fx.create_buffer("splines PrimitiveFade", 16);
        fx.write_buffer(fade, &bytes(&fn_matshader::frame::fade_buffer()));
        self.shared = Some(SharedBuffers { view, primitive: prim, sprite_vf, light });
        self.fade = Some(fade);
    }

    /// Every LOD of the cooked mesh in the local vertex layout (the packing of statics_render `kind`).
    fn mesh(&mut self, fx: &mut FxRenderer, object: &str) -> Option<usize> {
        if let Some(m) = self.mesh_of.get(object) { return *m; }
        let r = (|| -> Result<MeshG, String> {
            let long = fn_fx::data::long_package(object).to_owned();
            let (a, x, b) = fn_fx::data::package_files(&self.set, &long)?;
            let p = fn_ueio::Package::parse(&a, &x, &b).map_err(|e| format!("{long}: {e}"))?;
            let m = fn_mesh::StaticMesh::parse_named(&p, object.split_once('.').map(|x| x.1)).map_err(|e| format!("{long}: {e}"))?;
            let mut lods = vec![];
            for (li, lod) in m.lods.iter().enumerate() {
                let num_uv = lod.num_tex_coords;
                let stride = fn_fx::gpu::mesh_stride(Vf::SplineMesh, num_uv) as usize;
                let n = lod.positions.len();
                let mut vb = vec![0u8; n * stride];
                for i in 0..n {
                    let o = i * stride;
                    for (k, f) in lod.positions[i].iter().enumerate() { vb[o + 4 * k..o + 4 * k + 4].copy_from_slice(&f.to_le_bytes()); }
                    vb[o + 12..o + 16].copy_from_slice(&lod.tangent_x[i]);
                    vb[o + 16..o + 20].copy_from_slice(&lod.tangent_z[i]);
                    vb[o + 20..o + 24].copy_from_slice(&lod.colors.get(i).copied().unwrap_or([255; 4]));
                    for u in 0..num_uv {
                        let uv = lod.uvs.get(u).and_then(|s| s.get(i)).copied().unwrap_or([0.; 2]);
                        let at = o + 24 + 16 * (u / 2) + 8 * (u % 2);
                        vb[at..at + 4].copy_from_slice(&uv[0].to_le_bytes()); vb[at + 4..at + 8].copy_from_slice(&uv[1].to_le_bytes());
                    }
                }
                let mesh = fx.create_mesh(&FxMeshDesc { label: format!("{} LOD{li} (spline)", m.name), vertices: vb, indices: lod.indices.clone() });
                lods.push(Lod { mesh, num_uv, sections: lod.sections.iter().map(|s| (s.material_index.max(0) as usize, s.first_index, s.num_triangles * 3)).collect() });
            }
            if lods.is_empty() { return Err(format!("{long}: no LOD")); }
            // `UStaticMesh::GetBounds()` = ExtendedBounds (what CalculateScaleZAndMinZ / CalcBounds read), else the render-data bounds
            let bounds = fn_fx::data::PackageJson::load(&self.set, &long).ok().and_then(|pk| {
                let e = pk.of_class("StaticMesh").next()?.props.get("ExtendedBounds")?.clone();
                let v = |k: &str| -> Option<[f32; 3]> { let a = e.get(k)?.as_array()?; Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32]) };
                Some(BoxSphereBounds { origin: v("Origin")?, extent: v("BoxExtent")?, radius: e.get("SphereRadius")?.as_f64()? as f32 })
            }).unwrap_or(BoxSphereBounds { origin: m.bounds.origin, extent: m.bounds.extent, radius: m.bounds.radius });
            Ok(MeshG { lods, mats: m.materials.iter().map(|x| x.material.clone()).collect(), screen_size: m.screen_size, bounds })
        })();
        let r = match r { Ok(m) => { self.note(format!("mesh {}: {} LODs, bounds origin {:?} extent {:?}", object.rsplit('.').next().unwrap_or(""), m.lods.len(), m.bounds.origin, m.bounds.extent)); self.meshes.push(m); Some(self.meshes.len() - 1) } Err(e) => { self.note(format!("mesh {object}: NOT BUILT ({e})")); None } };
        self.mesh_of.insert(object.to_owned(), r);
        r
    }

    /// The `FSplineMeshVertexFactory` base-pass pipeline of a material (+ its shadow-depth pair), once per (material, UV count, culling).
    fn entry(&mut self, fx: &mut FxRenderer, path: &str, num_uv: usize, front_cw: bool) -> Option<usize> {
        let key = (path.to_owned(), num_uv, front_cw);
        if let Some(e) = self.entry_of.get(&key) { return *e; }
        let (shared, fade) = (self.shared.unwrap(), self.fade.unwrap());
        let r = (|| -> Result<usize, String> {
            let mat = Arc::new(Material::load(&self.set, path)?);
            let vf = Vf::SplineMesh;
            let ps = PS_TYPES.iter().copied().find(|ps| mat.cooked.find(vf.type_name(), ps).is_some()).ok_or_else(|| format!("no {} / {} program (MATUSAGE_SplineMesh missing)", vf.type_name(), PS_TYPES[0]))?;
            let gpu = build_material_layout(&self.set, fx, &mut self.cache, &mat, vf, VS_TYPE, ps, num_uv, false, shared, &[("PrimitiveFade", fade)], LayoutOpts { front_cw, ..LayoutOpts::default() })?;
            let vs_ok = gpu.vs_globals().map_or(false, |g| spline_vf_slots(&g.params).is_some());
            if !vs_ok { return Err("the base-pass VS has no spline-VF globals (layout did not validate)".into()); }
            // the velocity rule of statics_render: a Static component writes velocity only with a position-modifying material
            let velocity = fn_fx::data::package_files(&self.set, &mat.shader_map_package).ok().and_then(|(a, x, b)| { let p = fn_ueio::Package::parse(&a, &x, &b).ok()?; fn_fx::uniform::compilation_flags(&x, &p.names).map(|f| f.modifies_mesh_position) }).unwrap_or(false);
            let shadow = if mat.flags.blend == "BLEND_Opaque" || mat.flags.blend == "BLEND_Masked" {
                match fn_fx::gpu::build_shadow(&self.set, fx, &mut self.cache, &mat, None, vf, num_uv, shared, &[("PrimitiveFade", fade)], front_cw) {
                    Ok(Some((g, true))) if g.vs_globals().map_or(false, |g| spline_vf_slots(&g.params).is_some()) => Some(g),
                    Ok(_) => { self.note(format!("{path}: no own spline shadow-depth programs: casts no shadow")); None }
                    Err(e) => { self.note(format!("{path}: shadow NOT built ({e})")); None }
                }
            } else { None };
            self.note(format!("material {} (shader map {}): {} {VS_TYPE} / {ps}, blend {}, two-sided {}, velocity {velocity}, shadow {}{}", path.rsplit('.').next().unwrap_or(path), mat.shader_map_package.rsplit('/').next().unwrap_or(""), vf.type_name(), mat.flags.blend, mat.flags.two_sided, shadow.is_some(), if front_cw { ", reversed culling" } else { "" }));
            self.entries.push(Entry { mat, gpu, velocity });
            self.shadows.push(shadow);
            Ok(self.entries.len() - 1)
        })();
        let r = match r { Ok(e) => Some(e), Err(e) => { self.note(format!("material {path}: NOT BUILT ({e}): its sections are not drawn")); None } };
        self.entry_of.insert(key, r);
        r
    }

    /// Reads the export and builds every component's GPU data (once).
    fn build(&mut self, fx: &mut FxRenderer) { while !self.build_step(fx, None) {} }
    fn build_step(&mut self, fx: &mut FxRenderer, budget: Option<std::time::Duration>) -> bool {
        if self.built { return true; }
        let t0 = std::time::Instant::now();
        if self.pending.is_none() {
        // the POI mapping (poi.rs `load`) must exist first: until then nothing is placed and the build is retried (FN_POI=0 = never: no spline meshes)
        let Some(off) = crate::poi::current().map(|s| s.off) else { return true };
        let doc: Value = match std::fs::read(&self.file).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string())) { Ok(d) => d, Err(e) => { self.built = true; self.note(format!("{}: {e}", self.file.display())); return true; } };
        self.pending = Some((parse_components(&doc).into(), off, 0.0));
        }
        // `off` = the POI mapping: FX space = UE world about the layout centre moved to the game anchor, height minus the landscape ground reference
        loop {
            if budget.is_some_and(|b| t0.elapsed() >= b) { if let Some(p) = self.pending.as_mut() { p.2 += t0.elapsed().as_secs_f32() * 1000.; } return false; }
            let Some((c, off)) = self.pending.as_mut().and_then(|p| p.0.pop_front().map(|c| (c, p.1))) else { break };
            let Some(mi) = self.mesh(fx, &c.mesh) else { continue };
            let l2w = off * Mat4::from_scale_rotation_translation(c.s, c.q, c.t);
            let mb = self.meshes[mi].bounds;
            let local = c.spline.local_bounds(&mb);
            let values = c.spline.shader_values(&mb);
            let reverse = (Mat3::from_mat4(l2w).determinant() < 0.) ^ c.spline.flips_culling();
            let mut secs = vec![];
            for li in 0..self.meshes[mi].lods.len() {
                let (num_uv, sections) = (self.meshes[mi].lods[li].num_uv, self.meshes[mi].lods[li].sections.clone());
                let mut v = vec![];
                for (slot, _, _) in sections {
                    let path = c.overrides.get(slot).cloned().flatten().or_else(|| self.meshes[mi].mats.get(slot).cloned().flatten());
                    let e = path.and_then(|p| {
                        let e0 = self.entry(fx, &p, num_uv, false)?;
                        Some(if reverse && !self.entries[e0].mat.flags.two_sided { self.entry(fx, &p, num_uv, true).unwrap_or(e0) } else { e0 })
                    });
                    v.push((e, e.filter(|&e| c.cast_shadow && self.shadows[e].is_some())));
                }
                secs.push(v);
            }
            self.placed.push(Placed { c, mesh: mi, l2w, local, values, reverse, secs, prim: None, vs: HashMap::new(), binds: HashMap::new() });
        }
        self.built = true;
        let ms = t0.elapsed().as_secs_f32() * 1000. + self.pending.take().map_or(0.0, |p| p.2);
        let n = self.placed.len();
        self.note(format!("{n} spline mesh components placed ({} meshes, {} material entries) in {ms:.0} ms", self.meshes.len(), self.entries.len()));
        true
    }

    pub fn frame(&mut self, fx: &mut FxRenderer, view: &ViewParams, out: &mut FxFrame) {
        self.init_gpu(fx);
        self.build(fx);
        let shared = self.shared.unwrap();
        out.buffer_writes.push((shared.view, bytes(&view.view_buffer())));
        out.buffer_writes.push((shared.light, bytes(&fn_render::sky_light::fx_light_block())));
        let t = view.material_time();
        let eye = fn_fx::view::to_ue(view.eye).as_vec3();
        let fwd = fn_fx::view::dir_to_ue(view.forward).as_vec3().normalize();
        let up0 = fn_fx::view::dir_to_ue(view.up).as_vec3();
        let right = fwd.cross(up0).normalize();
        let up = right.cross(fwd).normalize();
        let (tx, ty) = view.tan_half;
        let (sx, sy) = ((1. + tx * tx).sqrt(), (1. + ty * ty).sqrt());
        let (near_cm, far_cm) = (view.near_m * 100., view.far_m * 100.);
        let occ = fn_render::occlusion::latest();
        let lod_scale = crate::statics_render::lod_scale();
        let mut st = Stats { components: self.placed.len(), ..Default::default() };
        // the Material buffers / pixel globals once per entry per frame; the shadow globals' non-vertex part too
        let mut used = vec![false; self.entries.len()];
        let mut shadow_writes: Vec<Option<Vec<(bool, FxBufId, Vec<u8>)>>> = vec![None; self.entries.len()];
        for pi in 0..self.placed.len() {
            let (l2w, local) = (self.placed[pi].l2w, self.placed[pi].local);
            let wb = local.transform_by(&l2w);
            let (c, r) = (Vec3::from(wb.origin), wb.radius);
            let d = c - eye;
            let dist = d.length();
            let cull_cm = self.placed[pi].c.cull_cm;
            if cull_cm > 0. && dist > cull_cm + r { st.culled_distance += 1; continue; }
            let z = d.dot(fwd);
            let in_frustum = z + r > near_cm && z - r < far_cm && d.dot(right).abs() <= z * tx + r * sx && d.dot(up).abs() <= z * ty + r * sy;
            let occluded = in_frustum && occ.as_ref().is_some_and(|o| o.sphere_occluded(Vec3::new(c.x, c.z, c.y) / 100., r / 100., view.eye));
            let in_view = in_frustum && !occluded;
            let cast = self.placed[pi].c.cast_shadow && dist - r < CSM_CM;
            if occluded { st.culled_occlusion += 1; } else if !in_frustum { st.culled_frustum += 1; }
            if !in_view && !cast { continue; }
            if in_view { st.drawn += 1; }
            let mi = self.placed[pi].mesh;
            let li = crate::statics_render::static_mesh_lod(&self.meshes[mi].screen_size, self.meshes[mi].lods.len(), c, r, eye, (tx, ty), lod_scale);
            // the component's Primitive buffer (static: written once; LocalObjectBounds = the deformed CalcBounds)
            let prim = match self.placed[pi].prim { Some(b) => b, None => {
                let rec = fn_render::primitive_ub::mesh_primitive(&l2w, &local, l2w.w_axis.truncate().to_array(), &fn_render::primitive_ub::PrimitiveFlags::default());
                let b = fx.create_buffer("splines primitive", rec.len() * 16);
                fx.write_buffer(b, &bytes(&rec));
                self.placed[pi].prim = Some(b); b } };
            let lod_mesh = self.meshes[mi].lods[li].mesh;
            let sections = self.meshes[mi].lods[li].sections.clone();
            for (si, &(_, first, count)) in sections.iter().enumerate() {
                let (e, se) = self.placed[pi].secs[li][si];
                let Some(e) = e else { continue };
                if !used[e] {
                    used[e] = true;
                    let en = &self.entries[e];
                    let packed = fn_fx::uniform::evaluate(&en.mat.uniform, &en.mat.overrides, t.world);
                    let mut cb = packed.cb; cb.resize(cb.len().max(en.gpu.material_registers).max(1), [0.0; 4]);
                    out.buffer_writes.push((en.gpu.material_buf, bytes(&cb)));
                    out.buffer_writes.extend(en.gpu.per_frame_writes(&en.mat, t));
                }
                if in_view {
                    // the per-component vertex globals: per-frame expressions + velocity globals + the spline values
                    let g = self.entries[e].gpu.vs_globals().unwrap().clone();
                    let vb = *self.placed[pi].vs.entry((e, false)).or_insert_with(|| fx.create_buffer("splines base-pass VS globals", g.float4s * 16));
                    let prev = if self.entries[e].velocity { Some(l2w) } else { None };
                    if let Some(by) = self.entries[e].gpu.vs_globals_bytes(&self.entries[e].mat, t, prev) {
                        let mut f = to_floats(&by); write_spline(&mut f, &g, &self.placed[pi].values); out.buffer_writes.push((vb, to_bytes(&f)));
                    }
                    let binds = match self.placed[pi].binds.get(&(e, false)) { Some(b) => b.clone(), None => { let b = self.entries[e].gpu.bind_variant_globals(fx, prim, None, Some(vb)); self.placed[pi].binds.insert((e, false), b.clone()); b } };
                    out.draws.push(FxDraw { pipe: self.entries[e].gpu.pipe, binds, geom: FxGeom::MeshRange { mesh: lod_mesh, first_index: first, index_count: count }, instances: None, sort: view.sort(wb.origin) });
                    st.draws += 1;
                }
                if let (true, Some(se)) = (cast, se) {
                    let sg = self.shadows[se].as_ref().unwrap();
                    let w = shadow_writes[se].get_or_insert_with(|| {
                        let flags: Vec<bool> = sg.globals.iter().map(|g| g.vertex).collect();
                        let ws = sg.shadow_globals_writes(&self.entries[se].mat, t, view.eye.to_array());
                        let mut v = vec![];
                        for (k, (b, by)) in ws.into_iter().enumerate() { let vx = flags.get(k).copied().unwrap_or(false); if !vx { out.buffer_writes.push((b, by.clone())); } v.push((vx, b, by)); }
                        v
                    }).clone();
                    let g = sg.vs_globals().unwrap().clone();
                    let Some((_, _, vby)) = w.iter().find(|(vx, b, _)| *vx && *b == g.buf) else { continue };
                    let vb = *self.placed[pi].vs.entry((se, true)).or_insert_with(|| fx.create_buffer("splines shadow VS globals", g.float4s * 16));
                    let mut f = to_floats(vby); write_spline(&mut f, &g, &self.placed[pi].values); out.buffer_writes.push((vb, to_bytes(&f)));
                    let mb = self.entries[se].gpu.material_buf;
                    let binds = match self.placed[pi].binds.get(&(se, true)) { Some(b) => b.clone(), None => { let b = self.shadows[se].as_ref().unwrap().bind_variant_globals(fx, prim, Some(mb), Some(vb)); self.placed[pi].binds.insert((se, true), b.clone()); b } };
                    out.shadow_draws.push(FxShadowDraw { pipe: self.shadows[se].as_ref().unwrap().pipe, binds, geom: FxGeom::MeshRange { mesh: lod_mesh, first_index: first, index_count: count }, bounds: Some([c.x / 100., c.z / 100., c.y / 100., r / 100.]), instances: None });
                    st.shadow_draws += 1;
                }
            }
        }
        self.stats = st;
    }

    pub fn report(&self) -> String {
        if self.log.is_empty() { return String::new(); }
        let s = &self.stats;
        format!("spline meshes (landscape splines, FSplineMeshVertexFactory): {} components, {} drawn, {} frustum / {} distance / {} occlusion culled, {} draws, {} shadow draws\n  {}\n", s.components, s.drawn, s.culled_frustum, s.culled_distance, s.culled_occlusion, s.draws, s.shadow_draws, self.log.join("\n  "))
    }
}

// ---- app glue ----------------------------------------------------------------------------------------------------------------------------------
static STATE: Mutex<Option<SplineRender>> = Mutex::new(None);

/// Opens the paks + finds the export once (no-op when disabled / missing: logged).
pub fn init(install: Option<PathBuf>, roots: &[PathBuf]) {
    if !enabled() { eprintln!("[splines] FN_ROADS=0: no spline meshes"); return; }
    let (Some(install), Some(dir)) = (install, crate::props::find_dir(roots)) else { eprintln!("[splines] no install / props folder: no spline meshes"); return };
    match SplineRender::new(&install, &dir) { Ok(s) => *STATE.lock().unwrap() = Some(s), Err(e) => eprintln!("[splines] unavailable: {e}") }
}
/// LOAD TIME: the components' GPU data + materials built under the loading screen (Finishing stage, as `statics_render::prebuild_step`), not in the first
/// frame of play. Always true (one shot, about 1 s; nothing to wait for when disabled or before the POI mapping exists: [`frame`] then builds it).
pub fn prebuild_step(fx: &mut FxRenderer) -> bool {
    if let Ok(mut g) = STATE.lock() { if let Some(s) = g.as_mut() { s.init_gpu(fx); return s.build_step(fx, Some(std::time::Duration::from_millis(40))); } }
    true
}
/// The frame's spline-mesh draws (no-op before [`init`]).
pub fn frame(fx: &mut FxRenderer, view: &ViewParams, out: &mut FxFrame) {
    let Ok(mut g) = STATE.lock() else { return };
    if let Some(s) = g.as_mut() { s.frame(fx, view, out); }
}
pub fn report() -> String { STATE.lock().ok().and_then(|g| g.as_ref().map(|s| s.report())).unwrap_or_default() }

#[cfg(test)]
mod tests {
    use super::*;
    const STREAM_BOUNDS: BoxSphereBounds = BoxSphereBounds { origin: [256., -512., -48.], extent: [256., 512., 48.], radius: 574.4423 };
    /// LS_01 SplineMeshComponent_0 (the cooked values; RelativeLocation (45218.6, -17785.6, -265.3)).
    fn ls01_0() -> SplineMesh {
        SplineMesh { p: SplineParams { start_pos: Vec3::new(-705.0078, -701.0859, 0.9248657), start_tangent: Vec3::new(1446.6022, 1394.6478, -2.5728543), start_scale: Vec2::new(4., 1.), start_roll: -0.003464083, start_offset: Vec2::new(2048., 0.12382139),
            end_pos: Vec3::new(705.0078, 701.0859, -0.9248657), end_tangent: Vec3::new(1358.0717, 1382.8191, -0.954423), end_scale: Vec2::new(4., 1.), end_roll: -0.004602138, end_offset: Vec2::new(2048., 0.1645004) }, ..SplineMesh::default() }
    }
    fn close(a: Vec3, b: Vec3, tol: f32) -> bool { (a - b).length() <= tol }

    #[test] fn hermite_matches_the_closed_form() {
        let (sp, st, ep, et) = (Vec3::new(1., 2., 3.), Vec3::new(10., -4., 2.), Vec3::new(7., 1., -1.), Vec3::new(3., 5., 0.));
        assert!(close(eval_pos(sp, st, ep, et, 0.), sp, 1e-5));
        assert!(close(eval_pos(sp, st, ep, et, 1.), ep, 1e-5));
        // h00(.5) = h01(.5) = 1/2, h10(.5) = 1/8, h11(.5) = -1/8
        assert!(close(eval_pos(sp, st, ep, et, 0.5), (sp + ep) * 0.5 + (st - et) * 0.125, 1e-5));
        assert!(close(eval_dir(sp, st, ep, et, 0.), st.normalize(), 1e-5));
        assert!(close(eval_dir(sp, st, ep, et, 1.), et.normalize(), 1e-5));
        // the derivative by central difference
        let a = 0.37; let h = 1e-3;
        let num = (eval_pos(sp, st, ep, et, a + h) - eval_pos(sp, st, ep, et, a - h)).normalize();
        assert!(close(eval_dir(sp, st, ep, et, a), num, 1e-3));
    }
    #[test] fn smooth_step_is_ues() {
        assert_eq!(smooth_step(0., 1., -0.5), 0.); assert_eq!(smooth_step(0., 1., 1.), 1.); assert!((smooth_step(0., 1., 0.5) - 0.5).abs() < 1e-6); assert!((smooth_step(0., 1., 0.25) - 0.15625).abs() < 1e-6);
    }
    #[test] fn scale_z_and_min_z_of_the_stream_mesh() {
        let (s, m) = SplineMesh::default().scale_z_min_z(&STREAM_BOUNDS);
        assert!((s - 1. / 512.).abs() < 1e-9 && m.abs() < 1e-6, "{s} {m}");
        let b = SplineMesh { boundary_min: 100., boundary_max: 300., ..SplineMesh::default() }.scale_z_min_z(&STREAM_BOUNDS);
        assert!((b.0 - 0.005).abs() < 1e-7 && (b.1 - 0.5).abs() < 1e-6);
    }
    #[test] fn the_slice_frame_is_ues_with_roll_offset_and_scale() {
        // straight spline along +X, up Z: BaseX = Z x X = +Y, BaseY = X x Y = +Z
        let s = SplineMesh { p: SplineParams { start_pos: Vec3::ZERO, start_tangent: Vec3::new(100., 0., 0.), end_pos: Vec3::new(100., 0., 0.), end_tangent: Vec3::new(100., 0., 0.), start_scale: Vec2::new(2., 3.), end_scale: Vec2::new(2., 3.), start_offset: Vec2::new(5., 7.), end_offset: Vec2::new(5., 7.), start_roll: 0., end_roll: 0. }, ..SplineMesh::default() };
        let m = s.slice_transform(0.5);
        assert!(close(m.transform_point3(Vec3::ZERO), Vec3::new(50., 5., 7.), 1e-4));
        assert!(close(m.transform_point3(Vec3::new(0., 1., 0.)), Vec3::new(50., 7., 7.), 1e-4), "mesh Y -> BaseX * scale.x");
        assert!(close(m.transform_point3(Vec3::new(0., 0., 1.)), Vec3::new(50., 5., 10.), 1e-4), "mesh Z -> BaseY * scale.y");
        // roll 90 deg: XVec = cos*BaseX - sin*BaseY = -Z, YVec = cos*BaseY + sin*BaseX = +Y
        let r = SplineMesh { p: SplineParams { start_roll: std::f32::consts::FRAC_PI_2, end_roll: std::f32::consts::FRAC_PI_2, start_scale: Vec2::ONE, end_scale: Vec2::ONE, start_offset: Vec2::ZERO, end_offset: Vec2::ZERO, ..s.p }, ..s };
        assert!(close(r.slice_transform(0.).transform_point3(Vec3::new(0., 1., 0.)), Vec3::new(0., 0., -1.), 1e-4));
        assert!(close(r.slice_transform(0.).transform_point3(Vec3::new(0., 0., 1.)), Vec3::new(0., 1., 0.), 1e-4));
        // bSmoothInterpRollScale: the scale follows SmoothStep(alpha), the position does not
        let sm = SplineMesh { p: SplineParams { start_scale: Vec2::ONE, end_scale: Vec2::new(3., 3.), start_offset: Vec2::ZERO, end_offset: Vec2::ZERO, ..s.p }, smooth_interp_roll_scale: true, ..s };
        let y = sm.slice_transform(0.25).transform_point3(Vec3::new(0., 1., 0.)).y;
        assert!((y - (1. + 2. * 0.15625)).abs() < 1e-4, "{y}");
    }
    #[test] fn the_vertex_deformation_spans_the_spline() {
        let s = ls01_0();
        // the mesh's start / end faces (x = 0 / 512) land on StartPos / EndPos + Offset (unrolled frame: LandscapeSplines.cpp:2323 pre-rotates it by -Roll)
        // + the vertex's y * scale.x along the ROLLED XVec; the centre line y = -512 (x 4 = -2048) against StartOffset.X 2048 leaves 2048 * (1 - cos, sin) of the roll
        let at = |pos: Vec3, tan: Vec3, off: Vec2, roll: f32, sx: f32| { let d = tan.normalize(); let bx = Vec3::Z.cross(d).normalize(); let by = d.cross(bx).normalize(); let xv = roll.cos() * bx - roll.sin() * by; pos + off.x * bx + off.y * by + (-512. * sx) * xv };
        let p0 = s.deform(Vec3::new(0., -512., 0.), &STREAM_BOUNDS);
        let p1 = s.deform(Vec3::new(512., -512., 0.), &STREAM_BOUNDS);
        assert!(close(p0, at(s.p.start_pos, s.p.start_tangent, s.p.start_offset, s.p.start_roll, s.p.start_scale.x), 0.05), "{p0}");
        assert!(close(p1, at(s.p.end_pos, s.p.end_tangent, s.p.end_offset, s.p.end_roll, s.p.end_scale.x), 0.05), "{p1}");
        assert!(close(p0, s.p.start_pos, 8.), "within the roll's 7 cm of the spline: {p0}");
        // the full width: 1024 x scale 4 = 4096 cm across, perpendicular to the spline
        let a = s.deform(Vec3::new(256., 0., 0.), &STREAM_BOUNDS);
        let b = s.deform(Vec3::new(256., -1024., 0.), &STREAM_BOUNDS);
        assert!(((a - b).length() - 4096.).abs() < 1., "{}", (a - b).length());
        assert!((a - b).normalize().dot(eval_dir(s.p.start_pos, s.p.start_tangent, s.p.end_pos, s.p.end_tangent, 0.5)).abs() < 1e-3);
    }
    #[test] fn calc_bounds_contains_every_deformed_vertex_and_is_tight() {
        let s = ls01_0();
        let b = s.local_bounds(&STREAM_BOUNDS);
        let (lo, hi) = (Vec3::from(b.origin) - Vec3::from(b.extent), Vec3::from(b.origin) + Vec3::from(b.extent));
        let (mut slo, mut shi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for i in 0..=64 { for (y, z) in [(0., 0.), (-1024., 0.), (0., -96.), (-1024., -96.)] {
            let p = s.deform(Vec3::new(512. * i as f32 / 64., y, z), &STREAM_BOUNDS);
            assert!(p.cmpge(lo - Vec3::splat(0.5)).all() && p.cmple(hi + Vec3::splat(0.5)).all(), "{p} outside {lo} {hi}");
            slo = slo.min(p); shi = shi.max(p);
        } }
        assert!((slo - lo).abs().max_element() < 20. && (shi - hi).abs().max_element() < 20., "{slo} {shi} vs {lo} {hi}");
    }
    #[test] fn shader_values_follow_the_serialize_order() {
        let s = ls01_0();
        let v = s.shader_values(&STREAM_BOUNDS);
        assert_eq!(v[0], s.p.start_pos.to_array().to_vec());
        assert_eq!(v[2], vec![s.p.start_roll]);
        assert_eq!(v[10], vec![0., 0., 1.]);
        assert_eq!(v[11][0].to_bits(), 0, "bSmoothInterpRollScale false as u32 0");
        assert!((v[13][0] - 1. / 512.).abs() < 1e-9);
        assert_eq!((v[14].clone(), v[15].clone(), v[16].clone()), (vec![1., 0., 0.], vec![0., 1., 0.], vec![0., 0., 1.]));
    }
    /// The exported components chain end to end in world space: each one's end slice (alpha 1, the centre line) is another one's start slice (alpha 0),
    /// except at the ends of a spline. Validates the transform chain, the defaults and the parameters together (needs the exported file).
    #[test] fn exported_spline_meshes_join_in_world_space() {
        let f = PathBuf::from(r"C:\Users\Brady\Desktop\Fortnite Rust\FortniteRuntime\assets\props\splines\landscape_splines.json");
        let Ok(b) = std::fs::read(&f) else { eprintln!("skip: no {}", f.display()); return };
        let comps = parse_components(&serde_json::from_slice(&b).unwrap());
        assert_eq!(comps.len(), 139);
        let world = |c: &Component, a: f32| Mat4::from_scale_rotation_translation(c.s, c.q, c.t).transform_point3(c.spline.slice_transform(a).transform_point3(Vec3::new(0., -512., 0.)));
        let starts: Vec<Vec3> = comps.iter().map(|c| world(c, 0.)).collect();
        let joined = comps.iter().enumerate().filter(|(i, c)| { let e = world(c, 1.); starts.iter().enumerate().any(|(j, s)| j != *i && (*s - e).length() < 1.) }).count();
        // 24 segments: at most one free end per segment end -> the rest must join within 1 cm
        assert!(joined >= comps.len() - 24, "only {joined} of {} components join the next within 1 cm", comps.len());
    }
}
